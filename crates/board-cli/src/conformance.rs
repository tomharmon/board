//! Operator smoke test for this installed Rust implementation, not an upstream wire test.
use anyhow::{Context, Result, ensure};
use board_local::{Board, Filter, PostOptions, automation::*};
use serde_json::json;

pub fn run(js: bool) -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("self-test.db");
    let b = Board::open(&path)?;
    let mut checks = Vec::new();
    let opts = PostOptions {
        key: Some("root".into()),
        files: vec![("test.bin".into(), vec![0, 1, 255])],
        ..Default::default()
    };
    let root = b.post("checks/local", "probe", "hello", &opts)?;
    ensure!(b.post("checks/local", "probe", "hello", &opts)?.id == root.id);
    checks.push("idempotent post");
    ensure!(b.download_as("probe", root.attachments[0].id)?.1 == [0, 1, 255]);
    checks.push("attachment bytes");
    let after = PostOptions {
        after: Some(root.id),
        ..Default::default()
    };
    let reply = b.reply(root.id, "probe/agent", "reply", &after)?;
    ensure!(b.reply(root.id, "probe", "stale", &after).is_err());
    checks.push("newest guard");
    let (cursor, events) = b.event_page(
        root.id,
        10,
        &Filter {
            re: Some(root.id),
            ..Default::default()
        },
        None,
    )?;
    ensure!(
        events.iter().any(|e| e.cursor == reply.id)
            && b.event_page(cursor, 10, &Filter::default(), None)?
                .1
                .is_empty()
    );
    checks.push("event resume");
    add_watcher_as(
        &b,
        "probe",
        "posted",
        Filter {
            channel: Some("checks/local".into()),
            ..Default::default()
        },
        Some("message.posted"),
    )?;
    let claimable = create_task(
        &b,
        "probe",
        TaskTarget::New {
            channel: "checks/local",
            text: "claimed work",
            options: &Default::default(),
        },
        &[],
        None,
    )?;
    let c = claim(&b, "probe/a", claimable.id, 60)?;
    let peer = Board::open(&path)?;
    ensure!(claim(&peer, "probe/b", c.id, 60).is_err());
    checks.push("exclusive claim");
    let f = next_event(&b, "posted")?
        .context("expected watcher match")?
        .cursor;
    drop(peer);
    drop(b);
    let b = Board::open(&path)?;
    ensure!(
        next_event(&b, "posted")?
            .context("lost unacknowledged event")?
            .cursor
            == f
    );
    ack_event(&b, "posted", f)?;
    checks.push("watcher redelivery");
    let response = b.reply(f, "probe/posted", "received", &Default::default())?;
    ensure!(b.thread(f)?.last().is_some_and(|m| m.id == response.id));
    ensure!(next_event(&b, "posted")?.is_none());
    checks.push("event-driven reply");
    if js {
        println!(
            "{}",
            json!({"ok":true,"scope":"isolated-local-engine","passed":checks})
        );
    } else {
        for s in &checks {
            println!("ok  {s}");
        }
        println!(
            "{} local engine checks passed on a disposable database",
            checks.len()
        );
    }
    Ok(())
}
