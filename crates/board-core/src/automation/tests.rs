use super::*;
use crate::{Board, Filter, PostOptions};
use anyhow::Result;
use std::collections::BTreeMap;

fn create(board: &Board, needs: &[i64]) -> Result<Task> {
    create_task(
        board,
        "ann",
        TaskTarget::New {
            channel: "work",
            text: "write tests",
            options: &PostOptions::default(),
        },
        needs,
        None,
    )
}

#[test]
fn failed_creation_rolls_back_thread_and_event() -> Result<()> {
    let board = Board::memory()?;
    let start = board.cursor()?;
    assert!(create(&board, &[888]).is_err());
    assert_eq!(board.cursor()?, start);
    assert!(list_tasks(&board, None, None)?.is_empty());
    Ok(())
}

#[test]
fn atomic_plan_with_forward_edges_replays_and_refuses_cycles() -> Result<()> {
    let b = Board::memory()?;
    let mut items: Vec<PlanItem> = serde_json::from_value(serde_json::json!([
        {"text":"ship", "key":"ship", "needs":["build"]},
        {"text":"compile", "key":"build"}
    ]))?;
    let tasks = create_plan(&b, "ann", "work", &items, Some("release"))?;
    assert_eq!(tasks[0].state, "waiting");
    assert_eq!(tasks[0].needs, [tasks[1].id]);
    let head = b.cursor()?;
    assert_eq!(
        create_plan(&b, "ann", "work", &items, Some("release"))?[0].id,
        tasks[0].id
    );
    items[1].needs.push(Prerequisite::Name("ship".into()));
    assert!(create_plan(&b, "ann", "work", &items, Some("release")).is_err());
    assert!(create_plan(&b, "ann", "work", &items, None).is_err());
    assert_eq!(head, b.cursor()?);
    Ok(())
}

#[test]
fn dependencies_leases_and_stale_epochs() -> Result<()> {
    let b = Board::memory()?;
    let first = create(&b, &[])?;
    let second = create(&b, &[first.id])?;
    assert_eq!(second.state, "waiting");
    assert!(
        change_task(
            &b,
            first.id,
            "ann",
            None,
            None,
            TaskAction::Needs(&[second.id])
        )
        .is_err()
    );
    let c = claim(
        &b,
        "ann/fix",
        ClaimTarget::Ready("work"),
        60,
        &BTreeMap::new(),
    )?;
    assert!(claim(&b, "ann/other", ClaimTarget::Id(c.id), 60, &BTreeMap::new()).is_err());
    assert!(
        change_task(
            &b,
            c.id,
            "ann",
            Some(c.epoch),
            c.claim_token.as_deref(),
            TaskAction::Complete(None)
        )
        .is_err()
    );
    b.conn
        .execute("UPDATE tasks SET lease_until=0 WHERE id=?", [c.id])?;
    let c2 = claim(&b, "ann/fix", ClaimTarget::Id(c.id), 60, &BTreeMap::new())?;
    assert!(
        change_task(
            &b,
            c.id,
            "ann/fix",
            Some(c.epoch),
            c.claim_token.as_deref(),
            TaskAction::Complete(None)
        )
        .is_err()
    );
    change_task(
        &b,
        c.id,
        "ann/fix",
        Some(c2.epoch),
        c2.claim_token.as_deref(),
        TaskAction::Complete(Some("done")),
    )?;
    assert_eq!(b.get(second.id)?.labels["board.task"], "ready");
    assert_eq!(list_tasks(&b, None, None)?.len(), 1);
    Ok(())
}

#[test]
fn concurrent_claims_have_one_winner() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("board.sqlite3");
    let id = create(&Board::open(&db)?, &[])?.id;
    let a = Board::open(&db)?;
    let b = Board::open(&db)?;
    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    let threads: Vec<_> = [("a", a), ("b", b)]
        .into_iter()
        .map(|(actor, b)| {
            let gate = gate.clone();
            std::thread::spawn(move || {
                gate.wait();
                claim(&b, actor, ClaimTarget::Id(id), 60, &BTreeMap::new()).is_ok()
            })
        })
        .collect();
    assert_eq!(
        threads
            .into_iter()
            .map(|t| t.join().unwrap() as usize)
            .sum::<usize>(),
        1
    );
    Ok(())
}

#[test]
fn watcher_peek_redelivers_across_restart_and_cannot_skip() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("board.sqlite3");
    let b = Board::open(&db)?;
    b.post("dev", "ann", "old", &Default::default())?;
    add_watcher(
        &b,
        "ci",
        Filter {
            channel: Some("dev".into()),
            ..Default::default()
        },
        Some("message.posted"),
    )?;
    b.post("elsewhere", "ann", "noise", &Default::default())?;
    let first = b.post("dev", "ann", "new", &Default::default())?;
    let last = b.post("dev", "ann", "another", &Default::default())?;
    assert_eq!(next_event(&b, "ci")?.unwrap().cursor, first.id);
    drop(b);
    let b = Board::open(db)?;
    assert!(ack_event(&b, "ci", last.id).is_err());
    assert_eq!(next_event(&b, "ci")?.unwrap().cursor, first.id);
    ack_event(&b, "ci", first.id)?;
    assert_eq!(next_event(&b, "ci")?.unwrap().cursor, last.id);
    ack_event(&b, "ci", last.id)?;
    assert!(next_event(&b, "ci")?.is_none());
    Ok(())
}
