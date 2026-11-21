use anyhow::Result;
use board_local::{Board, Filter, PostOptions, automation::*};
use rusqlite::Connection;
use std::{
    collections::BTreeMap,
    sync::{Arc, Barrier},
    thread,
};

fn due() -> Timing {
    Timing {
        due: chrono::Utc::now().timestamp(),
        every: None,
        times: None,
        until: None,
    }
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
fn new_schedule_checks_past_time_but_restart_dispatches_old_persisted_deadlines() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("deadline.db");
    let b = Board::open(&path)?;
    let at = chrono::Utc::now().timestamp();
    let head = b.cursor()?;
    assert!(
        schedule(
            &b,
            "ada",
            Destination::Channel("ops"),
            "too late",
            Timing {
                due: at - 61,
                ..due()
            },
            Default::default()
        )
        .is_err()
    );
    assert_eq!(b.cursor()?, head);
    let s = schedule(
        &b,
        "ada",
        Destination::Channel("ops"),
        "due",
        Timing {
            due: at - 30,
            ..due()
        },
        Default::default(),
    )?;
    drop(b);
    Connection::open(&path)?.execute(
        "UPDATE schedules SET due=? WHERE id=?",
        [at - 7 * 86_400, s.id],
    )?;
    let restarted = Board::open(path)?;
    assert_eq!(dispatch_due(&restarted)?, 1);
    assert_eq!(dispatch_due(&restarted)?, 0);
    assert_eq!(get_schedule(&restarted, s.id)?.state, "done");
    Ok(())
}

#[test]
fn failed_schedule_commit_retries_once_even_with_competing_workers() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("board.db");
    let b = Board::open(&db)?;
    let root = b.post("inbox", "sam", "issue", &PostOptions::default())?;
    let detail = b.reply(root.id, "ada", "my follow up", &PostOptions::default())?;
    let s = schedule(
        &b,
        "ada/clock",
        Destination::Reply(detail.id),
        "ping",
        due(),
        BTreeMap::new(),
    )?;
    let high = b.cursor()?;
    // An error after the message, events, and send identity were inserted must roll everything back.
    Connection::open(&db)?.execute_batch(
        "CREATE TRIGGER pause_dispatch BEFORE UPDATE OF sent ON schedules
       BEGIN SELECT RAISE(ABORT,'storage error'); END",
    )?;
    assert!(dispatch_due(&b).is_err());
    assert_eq!(b.cursor()?, high);
    assert_eq!(get_schedule(&b, s.id)?.sent, 0);
    assert_eq!(b.thread(root.id)?.len(), 2);
    Connection::open(&db)?.execute_batch("DROP TRIGGER pause_dispatch")?;
    drop(b);
    let workers = [Board::open(&db)?, Board::open(&db)?];
    let gate = Arc::new(Barrier::new(2));
    let runs: Vec<_> = workers
        .into_iter()
        .map(|b| {
            let gate = gate.clone();
            thread::spawn(move || {
                gate.wait();
                dispatch_due(&b)
            })
        })
        .collect();
    let mut sent = 0;
    for run in runs {
        sent += run.join().unwrap()?
    }
    assert_eq!(sent, 1);
    let b = Board::open(db)?;
    let messages = b.thread(root.id)?;
    assert_eq!(messages.len(), 3);
    let m = &messages[2];
    assert_eq!(m.actor, "ada/clock");
    assert_eq!(m.re, Some(root.id));
    assert_eq!(m.reply_to, Some(detail.id));
    assert_eq!(m.schedule, Some(ScheduledSend { id: s.id, n: 0 }));
    let posted = b.events(high, 10)?.remove(0);
    assert_eq!(posted.message.as_ref(), Some(m));
    assert_eq!(posted.payload["schedule"]["id"], s.id);
    assert_eq!(get_schedule(&b, s.id)?.state, "done");
    assert_eq!(dispatch_due(&b)?, 0);
    Ok(())
}

#[test]
fn deleting_either_the_reply_target_or_root_cancels_a_schedule() -> Result<()> {
    let b = Board::memory()?;
    let root = b.post("work", "ada", "root", &PostOptions::default())?;
    let child = b.reply(root.id, "ada", "child", &PostOptions::default())?;
    let s1 = schedule(
        &b,
        "ada",
        Destination::Reply(child.id),
        "to child",
        due(),
        BTreeMap::new(),
    )?;
    b.delete(child.id, "ada", None)?;
    assert_eq!(get_schedule(&b, s1.id)?.state, "canceled");
    let other = b.reply(root.id, "ada", "child2", &PostOptions::default())?;
    let s2 = schedule(
        &b,
        "ada/clock",
        Destination::Reply(other.id),
        "to other",
        due(),
        BTreeMap::new(),
    )?;
    let s3 = schedule(
        &b,
        "ada/clock",
        Destination::Reply(root.id),
        "to root",
        due(),
        BTreeMap::new(),
    )?;
    b.delete(root.id, "ada", None)?;
    assert_eq!(get_schedule(&b, s2.id)?.state, "canceled");
    assert_eq!(get_schedule(&b, s3.id)?.state, "canceled");
    assert_eq!(dispatch_due(&b)?, 0);
    assert_eq!(
        b.events(0, 100)?
            .iter()
            .filter(|e| e.kind == "schedule.canceled")
            .count(),
        3
    );
    Ok(())
}

#[test]
fn recurring_sends_keep_ordinal_across_restarts_and_obey_stop_conditions() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("board.db");
    let b = Board::open(&path)?;
    let epoch = chrono::Utc::now().timestamp();
    let late = schedule(
        &b,
        "ada",
        Destination::Channel("digest"),
        "expired",
        Timing {
            due: epoch,
            every: Some(60),
            times: None,
            until: Some(epoch + 600),
        },
        BTreeMap::new(),
    )?;
    let recurring = schedule(
        &b,
        "ada/clock",
        Destination::Channel("digest"),
        "status",
        Timing {
            due: epoch,
            every: Some(60),
            times: Some(2),
            until: None,
        },
        BTreeMap::new(),
    )?;
    let clock = Connection::open(&path)?;
    clock.execute(
        "UPDATE schedules SET due=due-900,until_at=until_at-900 WHERE id=?",
        [late.id],
    )?;
    clock.execute(
        "UPDATE schedules SET due=due-120 WHERE id=?",
        [recurring.id],
    )?;
    assert!(cancel_schedule(&b, recurring.id, "other").is_err());
    assert_eq!(dispatch_due(&b)?, 1);
    assert_eq!(get_schedule(&b, late.id)?.state, "done");
    assert_eq!(get_schedule(&b, late.id)?.sent, 0);
    let first = get_schedule(&b, recurring.id)?;
    assert_eq!(first.state, "pending");
    assert_eq!(first.sent, 1);
    assert_eq!((first.due - epoch) % 60, 0);
    assert!(first.due > epoch);
    // Advance to the next slot without sleeping for a minute.
    Connection::open(&path)?.execute(
        "UPDATE schedules SET due=? WHERE id=?",
        [epoch - 60, recurring.id],
    )?;
    drop(b);
    let b = Board::open(path)?;
    assert_eq!(dispatch_due(&b)?, 1);
    assert_eq!(dispatch_due(&b)?, 0);
    let s = get_schedule(&b, recurring.id)?;
    assert_eq!(s.state, "done");
    assert_eq!(s.sent, 2);
    let messages = b.messages(&Filter::default(), 10)?;
    assert_eq!(messages.len(), 2);
    for (n, m) in messages.iter().enumerate() {
        assert_eq!(m.actor, "ada/clock");
        assert_eq!(
            m.schedule,
            Some(ScheduledSend {
                id: recurring.id,
                n: n as i64
            })
        );
        assert!(m.re.is_none());
    }
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
fn watcher_filters_use_event_time_not_the_current_thread() -> Result<()> {
    let b = Board::memory()?;
    let old = b.post("ops/ci", "ada", "old", &PostOptions::default())?;
    add_watcher(
        &b,
        "triage",
        Filter {
            channel: Some("ops/".into()),
            labels: BTreeMap::from([("state".into(), "new".into())]),
            ..Default::default()
        },
        None,
    )?;
    b.label(old.id, "ada", "state", Some("new"))?;
    let reply = b.reply(old.id, "sam", "follow-up", &PostOptions::default())?;
    b.label(old.id, "ada", "state", None)?;
    b.reply(old.id, "sam", "not in view", &PostOptions::default())?;
    let entered = next_event(&b, "triage")?.unwrap();
    assert_eq!(entered.kind, "labels.changed");
    ack_event(&b, "triage", entered.cursor)?;
    let inherited = next_event(&b, "triage")?.unwrap();
    assert_eq!(inherited.cursor, reply.id);
    ack_event(&b, "triage", inherited.cursor)?;
    let left = next_event(&b, "triage")?.unwrap();
    assert_eq!(left.kind, "labels.changed");
    ack_event(&b, "triage", left.cursor)?;
    assert!(next_event(&b, "triage")?.is_none());
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
fn watcher_ignores_own_writes_but_delivers_own_scheduled_posts() -> Result<()> {
    let db = Board::memory()?;
    let w = add_watcher_as(
        &db,
        "ada/desk",
        "clock",
        Filter::default(),
        Some("message.posted"),
    )?;
    assert_eq!(w.run_as.as_deref(), Some("ada/clock"));
    db.post("ops", "ada/clock", "not recursive", &Default::default())?;
    assert!(next_event(&db, "clock")?.is_none());
    schedule(
        &db,
        "ada/clock",
        Destination::Channel("ops"),
        "wake me",
        due(),
        BTreeMap::new(),
    )?;
    assert_eq!(dispatch_due(&db)?, 1);
    let ev = next_event(&db, "clock")?.unwrap();
    assert_eq!(ev.message.unwrap().text, "wake me");
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
fn v1_watcher_receipts_and_checkpoint_are_migrated_without_loss() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("old.db");
    let db = Board::open(&path)?;
    let watcher = add_watcher(&db, "old", Filter::default(), Some("message.posted"))?;
    let a = db.post("ops", "sam", "seen", &Default::default())?;
    let b = db.reply(a.id, "sam", "leased", &Default::default())?;
    let c = db.reply(a.id, "sam", "pending", &Default::default())?;
    drop(db);
    let old = Connection::open(&path)?;
    old.execute_batch("DROP TABLE watcher_queue; DROP TABLE watcher_scan;
       CREATE TABLE watcher_delivery(name TEXT PRIMARY KEY,pending INTEGER,receipt TEXT,lease_until INTEGER,last_ack INTEGER,last_receipt TEXT);
       PRAGMA user_version=1;")?;
    old.execute("UPDATE watchers SET cursor=? WHERE name='old'", [a.id])?;
    old.execute(
        "INSERT INTO watcher_delivery VALUES('old',?1,'active',?2,?3,'acked')",
        rusqlite::params![b.id, chrono::Utc::now().timestamp() + 120, a.id],
    )?;
    drop(old);
    let new = Board::open(&path)?;
    assert!(get_watcher(&new, &watcher.name)?.cursor >= a.id);
    assert!(next_event(&new, "old")?.is_none());
    ack_delivery(&new, "old", a.id, Some("acked"))?;
    ack_delivery(&new, "old", b.id, Some("active"))?;
    assert_eq!(next_event(&new, "old")?.unwrap().cursor, c.id);
    Ok(())
}

#[test]
fn assignments_cannot_enqueue_another_persons_instructions_without_opt_in() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("queue.db");
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
    // Even rows written by v1 must not jump ahead of a worker's own queue.
    Connection::open(&path)?.execute(
        "UPDATE tasks SET assignee='bo/agent' WHERE id=?",
        [foreign.id],
    )?;
    assert!(
        claim(
            &db,
            "bo/agent",
            ClaimTarget::Ready("ops"),
            60,
            &BTreeMap::new()
        )
        .unwrap_err()
        .is::<NothingReady>()
    );
    change_task(&db, foreign.id, "ada", None, None, TaskAction::Assign(None))?;
    let own = make("bo", Some("bo/agent"))?;
    let picked = claim_from(
        &db,
        "bo/agent",
        ClaimTarget::Ready("ops"),
        60,
        &BTreeMap::new(),
        &["ada".into()],
    )?;
    assert_eq!(picked.id, own.id);
    assert!(
        claim(
            &db,
            "bo/agent",
            ClaimTarget::Ready("ops"),
            60,
            &BTreeMap::new()
        )
        .is_err()
    );
    assert_eq!(
        claim_from(
            &db,
            "bo/agent",
            ClaimTarget::Ready("ops"),
            60,
            &BTreeMap::new(),
            &["ada".into()]
        )?
        .id,
        foreign.id
    );
    Ok(())
}

#[test]
fn queue_labels_are_exact_and_owner_opt_ins_name_people() -> Result<()> {
    let db = Board::memory()?;
    let task = create_task(
        &db,
        "ada",
        TaskTarget::New {
            channel: "ops/ci",
            text: "test exact filters",
            options: &PostOptions {
                labels: BTreeMap::from([("role".into(), "reviewer".into())]),
                ..Default::default()
            },
        },
        &[],
        None,
    )?;
    let try_claim = |value: &str, owners: &[String]| {
        claim_from(
            &db,
            "bo/worker",
            ClaimTarget::Ready("ops/"),
            60,
            &BTreeMap::from([("role".into(), value.into())]),
            owners,
        )
    };
    let high = db.cursor()?;
    for v in ["", "worker|reviewer"] {
        assert!(
            try_claim(v, &["ada".into()])
                .unwrap_err()
                .to_string()
                .contains("one exact")
        );
    }
    assert!(try_claim("reviewer", &["ada/profile".into()]).is_err());
    assert!(
        try_claim("worker", &["ada".into()])
            .unwrap_err()
            .is::<NothingReady>()
    );
    assert_eq!(db.cursor()?, high);
    assert_eq!(try_claim("reviewer", &["ada".into()])?.id, task.id);
    Ok(())
}
