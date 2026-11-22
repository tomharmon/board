use anyhow::Result;
use board_local::{Board, Filter, PostOptions, automation::*};
use rusqlite::Connection;
use std::{
    collections::BTreeMap,
    sync::{Arc, Barrier},
    thread,
};

#[test]
fn offline_watchers_match_author_families_and_canonical_mentions_at_the_event_not_at_pull()
-> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("matches.db");
    let b = Board::open(&path)?;
    b.set_actor_profile("person-7", "lead", "Jules")?;
    b.set_actor_profile("person-9", "reviewer", "Ari")?;
    let f = Filter {
        by: Some("lead".into()),
        mentions: Some("reviewer".into()),
        channel: Some("work".into()),
        ..Default::default()
    };
    add_watcher(&b, "review", f.clone(), Some("message.posted"))?;
    let accepted = b.post(
        "work",
        "person-7/agent",
        "review @[person-9]",
        &Default::default(),
    )?;
    assert_eq!(
        b.event_page(0, 10, &f, Some("message.posted"))?.1[0]
            .message
            .as_ref()
            .unwrap()
            .id,
        accepted.id
    );
    b.set_actor_profile("person-7", "retired", "Jules")?;
    b.set_actor_profile("person-9", "away", "Ari")?;
    b.post(
        "work",
        "person-7/agent",
        "not this @[person-9]",
        &Default::default(),
    )?;
    b.set_actor_profile("person-11", "lead", "Jo")?;
    b.set_actor_profile("person-12", "reviewer", "Mira")?;
    let next = b.post(
        "work",
        "person-11",
        "action @[person-12]",
        &Default::default(),
    )?;
    drop(b);
    let resumed = Board::open(path)?;
    for expected in [accepted.id, next.id] {
        let offer = lease_event(&resumed, "review", 60)?.expect("durable event-time handle match");
        assert_eq!(offer.event.message.as_ref().unwrap().id, expected);
        ack_delivery(&resumed, "review", offer.event.cursor, Some(&offer.receipt))?;
    }
    assert!(lease_event(&resumed, "review", 60)?.is_none());
    Ok(())
}

#[test]
fn conditional_ack_has_one_winner_and_only_its_proof_can_report_no_match_after_reopen() -> Result<()>
{
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("acks.db");
    let b = Board::open(&path)?;
    add_watcher(&b, "w", Filter::default(), Some("message.posted"))?;
    b.post("public", "sam", "First", &Default::default())?;
    let offered = lease_event(&b, "w", 60)?.unwrap();
    let cursor = offered.event.cursor;
    let proof = offered.receipt;
    let head = b.cursor()?;
    let gate = Arc::new(Barrier::new(2));
    let workers = [Board::open(&path)?, Board::open(&path)?]
        .into_iter()
        .map(|db| {
            let gate = gate.clone();
            let proof = proof.clone();
            thread::spawn(move || {
                gate.wait();
                settle_delivery_if(&db, "w", cursor, Some(&proof), DeliveryOutcome::Ack, true)
                    .map(|_| "ok")
                    .unwrap_or_else(|e| board_local::errors::classify(&e).1)
            })
        })
        .collect::<Vec<_>>();
    let mut results = workers
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect::<Vec<_>>();
    results.sort();
    assert_eq!(results, ["no_match", "ok"]);
    drop(b);
    let b = Board::open(&path)?;
    let strict = |cursor, proof: Option<&str>| {
        settle_delivery_if(&b, "w", cursor, proof, DeliveryOutcome::Ack, true)
    };
    assert_eq!(
        board_local::errors::classify(&strict(cursor, Some(&proof)).unwrap_err()).1,
        "no_match"
    );
    let wrong = strict(cursor, Some("another proof")).unwrap_err();
    let absent = strict(cursor + 1000, Some(&proof)).unwrap_err();
    assert_eq!(board_local::errors::classify(&wrong).1, "delivery_conflict");
    assert_eq!(wrong.to_string(), absent.to_string());
    assert_eq!(
        board_local::errors::classify(&strict(cursor, None).unwrap_err()).1,
        "receipt_required"
    );
    ack_delivery(&b, "w", cursor, Some(&proof))?;
    assert_eq!(b.cursor()?, head);
    assert!(lease_event(&b, "w", 60)?.is_none());

    b.post("public", "sam", "Retryable", &Default::default())?;
    let d = lease_event(&b, "w", 60)?.unwrap();
    settle_delivery_if(
        &b,
        "w",
        d.event.cursor,
        Some(&d.receipt),
        DeliveryOutcome::Error("retry"),
        true,
    )?;
    assert_eq!(
        board_local::errors::classify(&strict(d.event.cursor, Some(&d.receipt)).unwrap_err()).1,
        "delivery_conflict"
    );
    Connection::open(&path)?.execute(
        "UPDATE watcher_queue SET retry_at=0 WHERE cursor=?",
        [d.event.cursor],
    )?;
    let next = lease_event(&b, "w", 60)?.unwrap();
    strict(next.event.cursor, Some(&next.receipt))?;
    assert_eq!(
        board_local::errors::classify(&strict(d.event.cursor, Some(&d.receipt)).unwrap_err()).1,
        "delivery_conflict"
    );
    Ok(())
}

#[test]
fn leased_delivery_is_exclusive_expires_and_fences_stale_acknowledgements() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("board.db");
    let b = Board::open(&db)?;
    add_watcher(&b, "jobs", Filter::default(), Some("message.posted"))?;
    let first = b.post("tasks", "sam", "one", &PostOptions::default())?;
    let second = b.reply(first.id, "sam", "two", &PostOptions::default())?;
    let gate = Arc::new(Barrier::new(2));
    let runs: Vec<_> = [Board::open(&db)?, Board::open(&db)?]
        .into_iter()
        .map(|b| {
            let gate = gate.clone();
            thread::spawn(move || {
                gate.wait();
                lease_event(&b, "jobs", 60)
            })
        })
        .collect();
    let deliveries = runs
        .into_iter()
        .map(|r| r.join().unwrap())
        .collect::<Result<Vec<_>>>()?;
    let mut deliveries = deliveries.into_iter().flatten();
    let lease = deliveries.next().unwrap();
    assert!(deliveries.next().is_none());
    assert_eq!(lease.event.cursor, first.id);
    assert!(next_event(&b, "jobs")?.is_none());
    assert!(ack_event(&b, "jobs", first.id).is_err());
    Connection::open(&db)?.execute(
        "UPDATE watcher_queue SET lease_until=0 WHERE name='jobs' AND lease_until IS NOT NULL",
        [],
    )?;
    drop(b);
    let b = Board::open(&db)?;
    let retry = lease_event(&b, "jobs", 60)?.unwrap();
    assert_eq!(retry.event.cursor, lease.event.cursor);
    assert_ne!(retry.receipt, lease.receipt);
    assert!(ack_delivery(&b, "jobs", first.id, Some(&lease.receipt)).is_err());
    ack_delivery(&b, "jobs", first.id, Some(&retry.receipt))?;
    ack_delivery(&b, "jobs", first.id, Some(&retry.receipt))?;
    assert_eq!(next_event(&b, "jobs")?.unwrap().cursor, second.id);
    ack_event(&b, "jobs", second.id)?;
    ack_event(&b, "jobs", second.id)?;
    assert!(next_event(&b, "jobs")?.is_none());
    Ok(())
}

#[test]
fn watcher_lanes_run_independently_and_survive_poison_events() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("lanes.db");
    let board = Board::open(&path)?;
    add_watcher_as(
        &board,
        "ada",
        "review",
        Filter::default(),
        Some("message.posted"),
    )?;
    let a = board.post("ops", "ada", "thread a", &Default::default())?;
    let ar = board.reply(a.id, "ada", "later a", &Default::default())?;
    let b = board.post("ops", "ada", "thread b", &Default::default())?;
    let br = board.reply(b.id, "ada", "later b", &Default::default())?;
    let gate = Arc::new(Barrier::new(2));
    let pulls: Vec<_> = [Board::open(&path)?, Board::open(&path)?]
        .into_iter()
        .map(|db| {
            let gate = gate.clone();
            thread::spawn(move || {
                gate.wait();
                lease_event(&db, "review", 60)
            })
        })
        .collect();
    let mut receipts = BTreeMap::new();
    for pull in pulls {
        let d = pull.join().unwrap()?.unwrap();
        receipts.insert(d.event.cursor, d.receipt);
    }
    assert_eq!(receipts.keys().cloned().collect::<Vec<_>>(), [a.id, b.id]);
    assert!(next_event(&board, "review")?.is_none());
    settle_delivery(
        &board,
        "review",
        b.id,
        Some(&receipts[&b.id]),
        DeliveryOutcome::Skip,
    )?;
    let bb = next_event(&board, "review")?.unwrap();
    assert_eq!(bb.cursor, br.id);
    ack_event(&board, "review", br.id)?;
    assert!(get_watcher(&board, "review")?.cursor < a.id);
    settle_delivery(
        &board,
        "review",
        a.id,
        Some(&receipts[&a.id]),
        DeliveryOutcome::Error("check failed"),
    )?;
    assert!(next_event(&board, "review")?.is_none());
    drop(board);
    let db = Board::open(&path)?;
    for attempt in 2..=5 {
        Connection::open(&path)?.execute(
            "UPDATE watcher_queue SET retry_at=0 WHERE name='review' AND cursor=?",
            [a.id],
        )?;
        let d = lease_event(&db, "review", 60)?.unwrap();
        assert_eq!(
            (d.event.cursor, d.attempt, d.attempts),
            (a.id, attempt - 1, attempt)
        );
        assert_ne!(d.receipt, receipts[&a.id]);
        assert!(ack_delivery(&db, "review", a.id, Some(&receipts[&a.id])).is_err());
        settle_delivery(
            &db,
            "review",
            a.id,
            Some(&d.receipt),
            DeliveryOutcome::Error("check failed"),
        )?;
    }
    let failures = watcher_failures(&db, "review", 0, 10)?.1;
    assert_eq!(failures.len(), 1);
    assert_eq!(
        (
            failures[0].cursor,
            failures[0].thread_id,
            failures[0].attempts
        ),
        (a.id, Some(a.id), 5)
    );
    assert_eq!(failures[0].error, "check failed");
    assert_eq!(next_event(&db, "review")?.unwrap().cursor, ar.id);
    ack_event(&db, "review", ar.id)?;
    assert!(next_event(&db, "review")?.is_none());
    assert_eq!(get_watcher(&db, "review")?.cursor, db.cursor()?);
    Ok(())
}

#[test]
fn watcher_can_post_a_reply_to_an_event_without_feeding_back_into_itself() -> Result<()> {
    let db = Board::memory()?;
    let w = add_watcher_as(
        &db,
        "ada/desk",
        "responder",
        Filter::default(),
        Some("message.posted"),
    )?;
    let actor = w.run_as.unwrap();
    let trigger = db.post("ops", "sam", "ready for review", &Default::default())?;
    let event = next_event(&db, "responder")?.unwrap();
    assert_eq!(event.cursor, trigger.id);
    let reply = db.reply(trigger.id, &actor, "reviewed", &Default::default())?;
    ack_event(&db, "responder", event.cursor)?;
    assert_eq!(db.thread(trigger.id)?.last().unwrap().id, reply.id);
    assert!(next_event(&db, "responder")?.is_none());
    Ok(())
}

#[test]
fn interrupted_deliveries_resist_stale_ack_and_unblock_the_lane_at_ten() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("interrupted.db");
    let mut db = Board::open(&path)?;
    add_watcher(&db, "review", Filter::default(), Some("message.posted"))?;
    let root = db.post("ops", "sam", "first", &Default::default())?;
    let reply = db.reply(root.id, "sam", "second", &Default::default())?;
    for attempt in 1..=10 {
        let leased = lease_event(&db, "review", 60)?.unwrap();
        assert_eq!(
            (leased.attempt, leased.attempts, leased.event.cursor),
            (attempt - 1, attempt, root.id)
        );
        Connection::open(&path)?.execute(
            "UPDATE watcher_queue SET lease_until=0 WHERE name='review' AND cursor=?",
            [root.id],
        )?;
        drop(db);
        db = Board::open(&path)?;
        assert!(ack_delivery(&db, "review", root.id, Some(&leased.receipt)).is_err());
    }
    let failures = watcher_failures(&db, "review", 0, 20)?.1;
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].attempts, 10);
    assert_eq!(failures[0].error, "delivery lease expired");
    assert_eq!(next_event(&db, "review")?.unwrap().cursor, reply.id);
    Ok(())
}

#[test]
fn task_assignment_cannot_authorize_claims_by_another_persons_profiles() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("claims.db");
    let db = Board::open(&path)?;
    let make = |who: &str, assignee: Option<&str>| {
        create_task(
            &db,
            who,
            TaskTarget::New {
                channel: "ops",
                text: "review",
                options: &Default::default(),
            },
            &[],
            assignee,
        )
    };
    let start = db.cursor()?;
    assert!(make("ada", Some("bo/agent")).is_err());
    assert_eq!(db.cursor()?, start);
    let foreign = make("ada", None)?;
    assert!(
        change_task(
            &db,
            foreign.id,
            "ada",
            None,
            None,
            TaskAction::Assign(Some("bo/agent"))
        )
        .is_err()
    );
    let items: Vec<PlanItem> =
        serde_json::from_value(serde_json::json!([{"text":"one","for":"bo/agent"}]))?;
    assert!(create_plan(&db, "ada", "ops", &items, None).is_err());
    // A malformed legacy assignment still cannot confer claim rights.
    Connection::open(&path)?.execute(
        "UPDATE tasks SET assignee='bo/agent' WHERE id=?",
        [foreign.id],
    )?;
    assert!(claim(&db, "bo/agent", foreign.id, 60).is_err());
    Ok(())
}
