use anyhow::Result;
use board_local::{
    Board, Filter, PostOptions,
    access::Rules,
    automation::{ScheduledSend, export_legacy_schedules},
    errors::classify,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{fs::File, path::Path};

// Made with the v15 CLI: one private root, one recurring reply dispatched once with
// an upload and inline bytes, a pending one-shot and a canceled timer. The two pending
// due values were moved into the past before checkpointing and compressing the database.
fn fixture(path: &Path) -> Result<Connection> {
    let mut input =
        flate2::read::GzDecoder::new(&include_bytes!("fixtures/timers-v15.sqlite3.gz")[..]);
    std::io::copy(&mut input, &mut File::create(path)?)?;
    let c = Connection::open(path)?;
    assert_eq!(
        c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?,
        15
    );
    Ok(c)
}

fn export(board: &Board) -> Result<Value> {
    let mut bytes = Vec::new();
    export_legacy_schedules(board, &mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[test]
fn upgrade_fences_even_an_open_v15_worker_and_never_replays_overdue_sends() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("legacy.db");
    let old = fixture(&path)?;
    let due = "SELECT count(*) FROM schedules WHERE state='pending' AND due<=unixepoch()";
    let mut scan = old.prepare(due)?;
    assert_eq!(scan.query_row([], |r| r.get::<_, i64>(0))?, 2);
    let head: i64 = old.query_row("SELECT max(cursor) FROM events", [], |r| r.get(0))?;
    for _ in 0..2 {
        let b = Board::open(&path)?;
        assert_eq!(
            old.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?,
            16
        );
        assert!(scan.query_row([], |r| r.get::<_, i64>(0)).is_err());
        assert!(!old.table_exists(None, "schedules")?);
        let history = b.thread(5)?;
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].text, "existing root");
        let reply = &history[1];
        assert_eq!(reply.id, 7);
        assert_eq!(reply.schedule, Some(ScheduledSend { id: 1, n: 0 }));
        assert!(reply.no_sync);
        assert_eq!(reply.labels["phase"], "archive");
        assert_eq!(
            b.download(reply.attachments[0].id)?.1,
            b"previously uploaded bytes\n"
        );
        assert_eq!(b.download(reply.attachments[1].id)?.1, b"queued bytes\n");
        let archived = export(&b)?;
        assert_eq!(archived["format"], "board.retired-schedules.v1");
        let timers = archived["schedules"].as_array().unwrap();
        assert_eq!(timers.len(), 3);
        assert_eq!(timers[0]["re"], 5);
        assert_eq!(timers[0]["state_at_retirement"], "pending");
        assert_eq!(timers[0]["no_sync"], true);
        assert_eq!(timers[0]["labels"], json!({"phase":"archive"}));
        assert_eq!(timers[0]["every"], 3600);
        assert_eq!(timers[0]["max_sends"], 3);
        assert_eq!(timers[0]["sent"], 1);
        assert_eq!(timers[0]["sends"], json!([{"message_id":7,"n":0}]));
        assert_eq!(timers[0]["attachments"][0]["attachment_id"], 1);
        assert_eq!(
            timers[0]["attachments"][1]["data_hex"],
            "7175657565642062797465730A"
        );
        assert_eq!(timers[1]["text"], "never publish these private bytes");
        assert!(timers[1]["every"].is_null());
        assert_eq!(timers[1]["sent"], 0);
        assert_eq!(timers[2]["state_at_retirement"], "canceled");
        assert_eq!(timers[2]["ended_by"], "ada");
        assert_eq!(
            b.cursor()?,
            head,
            "migration, reopen and export write no events"
        );
        assert!(
            b.events(0, 100)?
                .iter()
                .any(|e| e.cursor == 7 && e.payload["schedule"] == json!({"id":1,"n":0}))
        );
        assert!(!old.prepare("PRAGMA foreign_key_check")?.exists([])?);
    }
    Ok(())
}

#[test]
fn archive_is_never_available_to_bearers_or_narrowed_owner_sessions() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("private.db");
    drop(fixture(&path)?);
    let mut b = Board::open(&path)?;
    b.set_grant("ada", None, "guest", &Rules::parse("public/=read")?)?;
    let credential = b.issue_credential("ada", "guest", None, None)?;
    for (token, cap) in [
        (Some(credential.token.as_str()), None),
        (None, Some("read")),
    ] {
        b.with_request(token, "ada", cap, |b, _| {
            let mut output = Vec::new();
            let denied = export_legacy_schedules(b, &mut output).unwrap_err();
            assert_eq!(classify(&denied).1, "owner_required");
            assert!(output.is_empty());
            if token.is_some() {
                assert!(b.thread(5).is_err());
                assert!(b.download(1).is_err());
                assert!(b.messages(&Filter::default(), 50)?.is_empty());
                assert!(
                    !b.events(0, 100)?
                        .iter()
                        .any(|e| e.kind.starts_with("schedule."))
                );
            }
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn draft_intended_for_later_requires_review_and_keeps_its_bytes_and_privacy() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("draft.db");
    let old = fixture(&path)?;
    old.execute_batch(r#"INSERT INTO composer_drafts VALUES('ada','thread:5','my unsent reply','archive',5);
        UPDATE composer_settings SET stamp='v15-stamp',settings=
            '{"no_sync":true,"labels":"phase=private","later":true,"due":"2001-01-01T12:00:00Z","every":"60","times":"2"}'
            WHERE actor='ada' AND scope='thread:5';
        INSERT INTO composer_files VALUES('ada','thread:5',0,'queued.bin',x'00FF');"#)?;
    let b = Board::open(&path)?;
    let held = b.load_composer("ada", "thread:5")?.unwrap();
    assert!(held.settings.review_before_send);
    assert_ne!(held.stamp, "v15-stamp");
    assert_eq!(held.files[0].1, [0, 255]);
    let post = PostOptions {
        no_sync: true,
        labels: held.settings.parsed_labels()?,
        files: held.files.clone(),
        ..Default::default()
    };
    let send =
        |stamp: &str| b.reply_composer(5, "ada", &held.draft.body, &post, ("thread:5", stamp));
    assert!(send(&held.stamp).is_err());
    assert!(send("v15-stamp").is_err());
    assert_eq!(b.thread(5)?.len(), 2);
    assert!(b.load_composer("ada", "thread:5")?.is_some());
    let mut reviewed = held.settings.clone();
    reviewed.review_before_send = false;
    let new_stamp = b
        .save_composer(
            "ada",
            "thread:5",
            Some(&held.stamp),
            Some((&held.draft, &reviewed)),
            None,
        )?
        .unwrap();
    assert!(send(&held.stamp).is_err());
    let message = send(&new_stamp)?;
    assert_eq!(message.text, "my unsent reply");
    assert!(message.no_sync);
    assert_eq!(message.labels["phase"], "private");
    assert_eq!(b.download(message.attachments[0].id)?.1, [0, 255]);
    assert!(message.schedule.is_none());
    assert!(send(&new_stamp).is_err());
    Ok(())
}
