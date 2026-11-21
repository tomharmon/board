use super::*;
use board_local::{ComposerSettings, Draft};

#[test]
fn pending_picker_rechecks_current_post_permission_before_reading_the_file() {
    use board_local::access::Rules;
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let root = b
        .post("ops", "owner", "Visible thread", &Default::default())
        .unwrap()
        .id;
    let actor = "reader/helper";
    let grant = b
        .set_grant(
            "owner",
            None,
            actor,
            &Rules::parse("ops=read,post").unwrap(),
        )
        .unwrap();
    let issued = b.issue_credential("owner", actor, None, None).unwrap();
    let mut cmd = f.command(
        json!([
            {"t":0.25,"type":"tap","key":"attach-file"},{"t":0.50,"type":"mark","name":"requested"},
            {"t":2.25,"type":"file-dialog","path":f.dir.path().join("nonexistent-private-file")}
        ]),
        DESKTOP,
        "2.7",
    );
    cmd.env("BOARD_TOKEN", &issued.token)
        .arg("--thread")
        .arg(root.to_string());
    let t = f.at_mark(&mut cmd, "requested", || {
        b.set_grant(
            "owner",
            Some(grant.id),
            actor,
            &Rules::parse("ops=read").unwrap(),
        )
        .unwrap();
    });
    t.expect_text("Visible thread");
    t.expect_text("Posting to this channel is no longer permitted");
    assert!(!t.has_key("queued-files"));
    assert_eq!(b.thread(root).unwrap().len(), 1);
}

#[test]
fn delayed_picker_cannot_attach_to_the_other_split_draft_or_a_reopened_copy() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let root = b
        .post("ops", "reader", "Plan", &Default::default())
        .unwrap()
        .id;
    let scope = format!("thread:{root}");
    for (key, body, channel, no_sync) in [
        ("new", "Public update", "news", false),
        (scope.as_str(), "Unpublished reply", "ops", true),
    ] {
        b.save_composer(
            "reader",
            key,
            None,
            Some((
                &Draft {
                    body: body.into(),
                    channel: channel.into(),
                    reply_to: None,
                },
                &ComposerSettings {
                    no_sync,
                    ..Default::default()
                },
            )),
            None,
        )
        .unwrap();
    }
    let file = f.dir.path().join("private-note.txt");
    fs::write(&file, "private bytes").unwrap();
    for switch_back in [false, true] {
        let mut steps = vec![
            json!({"t":0.2,"type":"tap","key":"attach-file"}),
            json!({"t":0.5,"type":"tap","key":"feed-compose"}),
        ];
        if switch_back {
            steps.push(json!({"t":0.8,"type":"tap","key":"thread-compose"}));
        }
        steps.push(json!({"t":1.0,"type":"file-dialog","path":file}));
        let out = f
            .command(Value::Array(steps), DESKTOP, "1.4")
            .arg("--thread")
            .arg(root.to_string())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let t = f.tree();
        t.expect_text("Composer changed; file was not attached");
        assert!(!t.has_key("queued-files"));
        for (key, private) in [("new", false), (scope.as_str(), true)] {
            let saved = b.load_composer("reader", key).unwrap().unwrap();
            assert!(saved.files.is_empty());
            assert_eq!(saved.settings.no_sync, private);
        }
        assert_eq!(b.thread(root).unwrap().len(), 1);
    }
}
