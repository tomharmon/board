use anyhow::Result;
use board_local::{
    Board, PostOptions,
    access::Rules,
    errors::{Kind, classify},
    swarm::*,
};
use std::{
    fs,
    sync::{Arc, Barrier},
};

fn start(b: &Board, actor: &str, max: i64) -> Result<SwarmRun> {
    b.swarm_start(
        actor,
        &SwarmStart {
            goal: "Fix the offline source editor".into(),
            channel: Some("editor".into()),
            slug: Some("editor".into()),
            max_notifications: Some(max),
            ..Default::default()
        },
    )
}
fn spec(name: &str) -> SwarmAddWorker {
    SwarmAddWorker {
        name: name.into(),
        brief: format!("Own the {name} acceptance slice"),
        ..Default::default()
    }
}
fn add(b: &Board, name: &str) -> Result<RegisteredWorker> {
    b.swarm_register_worker("ada", "editor", &spec(name))
}

#[test]
fn run_picker_counts_members_only_in_visible_runs_before_paginating() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("summary.db");
    let b = Board::open(&path)?;
    let public = start(&b, "ada", 20)?;
    b.swarm_register_worker("ada", "editor", &spec("check"))?;
    let private = b.swarm_start(
        "ada",
        &SwarmStart {
            goal: "Private work".into(),
            channel: Some("private".into()),
            slug: Some("private".into()),
            ..Default::default()
        },
    )?;
    b.set_grant("ada", None, "reader", &Rules::parse("editor=read")?)?;
    let mut reader = Board::open(&path)?;
    reader.bind_token(&b.issue_credential("ada", "reader", None, None)?.token)?;
    let view = reader.swarm_summaries("reader", None, 1)?;
    assert_eq!(view.len(), 1);
    assert_eq!(view[0].id, public.id);
    assert_eq!(view[0].worker_count, 1);
    assert!(
        reader
            .swarm_summaries("reader", Some(public.id), 1)?
            .is_empty()
    );
    assert!(reader.swarm_summaries("ada", None, 1).is_err());
    assert_eq!(b.swarm_summaries("ada", None, 1)?[0].id, private.id);
    Ok(())
}

#[test]
fn explicit_start_slug_survives_races_lost_responses_and_stop() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("start.db");
    let b = Board::open(&path)?;
    let args = SwarmStart {
        goal: "Verify startup".into(),
        slug: Some("startup".into()),
        channel: Some("cli".into()),
        ..Default::default()
    };
    let gate = Arc::new(Barrier::new(2));
    let starts: Vec<_> = [Board::open(&path)?, Board::open(&path)?]
        .into_iter()
        .map(|db| {
            let gate = gate.clone();
            let args = args.clone();
            std::thread::spawn(move || {
                gate.wait();
                db.swarm_start("ada", &args)
            })
        })
        .collect();
    let runs: Vec<_> = starts
        .into_iter()
        .map(|s| s.join().unwrap())
        .collect::<Result<_>>()?;
    assert_eq!(runs[0].id, runs[1].id);
    let thread = runs[0].thread_id;
    assert_eq!(thread, runs[1].thread_id);
    let events = b.events(0, 100)?;
    assert_eq!(
        events.iter().filter(|e| e.kind == "swarm.started").count(),
        1
    );
    assert_eq!(
        events.iter().filter(|e| e.kind == "message.posted").count(),
        1
    );
    b.swarm_set_limit("ada", "startup", 17)?;
    b.swarm_stop("ada", "startup")?;
    let head = b.cursor()?;
    drop(b);
    let b = Board::open(&path)?;
    let stopped = b.swarm_start(
        "ada",
        &SwarmStart {
            max_notifications: Some(200),
            ..args.clone()
        },
    )?;
    assert_eq!(stopped.id, runs[0].id);
    assert_eq!(stopped.max_notifications, 17);
    assert_eq!(stopped.status, "stopped");
    for changed in [
        SwarmStart {
            goal: "A different goal".into(),
            ..args.clone()
        },
        SwarmStart {
            max_notifications: Some(17),
            ..args.clone()
        },
        SwarmStart {
            repo_root: Some("/work".into()),
            ..args.clone()
        },
        SwarmStart {
            thread_id: Some(thread),
            channel: None,
            ..args.clone()
        },
    ] {
        assert_eq!(
            classify(&b.swarm_start("ada", &changed).unwrap_err()).1,
            "swarm_exists"
        );
    }
    assert_eq!(b.cursor()?, head);
    let linked = SwarmStart {
        thread_id: Some(thread),
        channel: None,
        slug: Some("second".into()),
        ..args
    };
    let second = b.swarm_start("ada", &linked)?;
    assert_ne!(second.id, stopped.id);
    assert_eq!(second.thread_id, thread);
    assert_eq!(b.swarm_start("ada", &linked)?.id, second.id);
    let unkeyed = SwarmStart {
        slug: None,
        ..linked
    };
    assert_ne!(
        b.swarm_start("ada", &unkeyed)?.id,
        b.swarm_start("ada", &unkeyed)?.id
    );
    Ok(())
}

#[test]
fn start_replay_recovers_a_run_after_its_linked_root_was_removed() -> Result<()> {
    let mut b = Board::memory()?;
    let root = b
        .post("work", "ada", "Investigate", &PostOptions::default())?
        .id;
    let req = SwarmStart {
        goal: "Investigate".into(),
        thread_id: Some(root),
        slug: Some("investigate".into()),
        ..Default::default()
    };
    let original = b.swarm_start("ada", &req)?;
    b.delete(root, "ada", None)?;
    let head = b.cursor()?;
    let replay = b.swarm_start("ada", &req)?;
    assert_eq!((replay.id, replay.thread_id), (original.id, root));
    b.with_request(None, "ada", Some("work=read"), |b, actor| {
        assert_eq!(
            classify(&b.swarm_start(actor, &req).unwrap_err()).1,
            "missing_permission"
        );
        Ok(())
    })?;
    assert_eq!(
        classify(&b.swarm_start("other", &req).unwrap_err()).1,
        "swarm_exists"
    );
    let fresh = SwarmStart {
        slug: Some("new".into()),
        ..req
    };
    assert_eq!(
        classify(&b.swarm_start("ada", &fresh).unwrap_err()).1,
        "invalid_thread"
    );
    assert_eq!(b.cursor()?, head);
    Ok(())
}

#[test]
fn local_owner_can_supervise_another_coordinator_without_taking_its_inbox() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("board.db");
    let owner = Board::open(&path)?;
    for actor in ["guest/chat", "guest/impl", "outsider"] {
        owner.set_grant("ada", None, actor, &Rules::parse("editor=read,post")?)?;
    }
    let delegated = |actor: &str| -> Result<Board> {
        let token = owner.issue_credential("ada", actor, None, None)?.token;
        let mut b = Board::open(&path)?;
        b.bind_token(&token)?;
        Ok(b)
    };
    let coordinator = delegated("guest/chat")?;
    start(&coordinator, "guest/chat", 5)?;
    coordinator.swarm_register_worker("guest/chat", "editor", &spec("impl"))?;
    let other = delegated("outsider")?;
    assert_eq!(
        classify(&start(&other, "outsider", 5).unwrap_err()).1,
        "swarm_exists"
    );
    assert_eq!(
        classify(&start(&owner, "ada", 5).unwrap_err()).1,
        "swarm_exists"
    );
    assert_eq!(start(&coordinator, "guest/chat", 5)?.workers.len(), 1);
    assert!(!other.swarm_status("outsider", "editor")?.can_control);
    assert!(owner.swarm_status("ada", "editor")?.can_control);
    assert!(other.swarm_stop("outsider", "editor").is_err());
    owner.swarm_register_worker("ada", "editor", &spec("verify"))?;
    owner.swarm_set_limit("ada", "editor", 10)?;
    assert!(
        owner
            .swarm_post("ada", "editor", "FYI @all", &PostOptions::default())?
            .notified
            .is_empty()
    );
    assert!(owner.swarm_wait_target("ada", "editor").is_err());
    let note =
        delegated("guest/impl")?.swarm_dm("guest/impl", "editor", "verify", "Private peer note")?;
    let visible = |b: &Board, who: &str| -> Result<bool> {
        Ok(b.swarm_read(who, "editor", 0, 10, true)?
            .entries
            .iter()
            .any(|e| e.cursor == note.event_cursor))
    };
    assert!(visible(&owner, "ada")?);
    assert!(!visible(&other, "outsider")?);
    assert_eq!(owner.swarm_stop("ada", "editor")?.status, "stopped");
    Ok(())
}

#[test]
fn read_wait_gap_and_private_dm_share_one_cursor_and_survive_restart() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("board.sqlite");
    let b = Board::open(&db)?;
    start(&b, "ada", 200)?;
    add(&b, "impl")?;
    add(&b, "verify")?;
    add(&b, "design")?;
    let after = b.swarm_read("ada/verify", "editor", 0, 100, false)?.cursor;
    let plain = b.swarm_post(
        "ada/impl",
        "editor",
        "verify is checking",
        &Default::default(),
    )?;
    assert!(plain.notified.is_empty());
    let code = b.swarm_post(
        "ada/impl",
        "editor",
        "An inline example: \u{60}@verify\u{60}",
        &Default::default(),
    )?;
    assert!(code.notified.is_empty());
    let dm = b.swarm_dm("ada/impl", "editor", "verify", "private command results")?;
    let key = PostOptions {
        key: Some("unique-shared-handoff".into()),
        ..Default::default()
    };
    let m = b.swarm_post(
        "ada/impl",
        "editor",
        "@verify @impl @verify please rerun",
        &key,
    )?;
    assert_eq!(m.notified, ["verify"]);
    let again = b.swarm_post(
        "ada/impl",
        "editor",
        "@design do not accidentally deliver this",
        &key,
    )?;
    assert_eq!(again.message.id, m.message.id);
    assert_eq!(again.notified, ["verify"]);
    assert!(dm.event_cursor < m.message.id);
    drop(b);
    let b = Board::open(&db)?;
    let WaitPoll::Ready {
        cursor,
        notifications,
        messages,
        dms,
    } = b.swarm_poll_wait("ada/verify", "editor", "verify", after)?
    else {
        panic!("lost gap wake")
    };
    assert_eq!(
        notifications
            .iter()
            .map(|n| n.source_kind.as_str())
            .collect::<Vec<_>>(),
        ["dm", "mention"]
    );
    assert_eq!(messages[0].id, m.message.id);
    assert_eq!(dms[0].id, dm.id);
    assert_eq!(cursor, m.message.id);
    let page = b.swarm_read("ada/verify", "editor", after, 3, false)?;
    assert_eq!(page.entries.last().unwrap().dm.as_ref().unwrap().id, dm.id);
    assert_eq!(
        b.swarm_read("ada/verify", "editor", page.cursor, 1, false)?
            .cursor,
        m.message.id
    );
    let filtered = b.swarm_latest_filtered(
        "ada",
        "editor",
        None,
        2,
        true,
        &SwarmFilter {
            kind: Some("dm".into()),
            worker: Some("verify".into()),
        },
    )?;
    assert_eq!(filtered.entries.len(), 1);
    assert!(
        b.swarm_read("ada/design", "editor", after, 100, true)?
            .entries
            .iter()
            .all(|e| e.dm.is_none())
    );
    assert!(
        b.swarm_read("ada", "editor", after, 100, false)?
            .entries
            .iter()
            .any(|e| e.dm.is_some())
    );
    assert!(
        b.event_feed(0, 500, &Default::default(), Some("swarm.dm"), true)?
            .1
            .is_empty()
    );
    let sql = rusqlite::Connection::open(&db)?;
    let leak:i64=sql.query_row("SELECT count(*) FROM events WHERE instr(payload,'private command results') OR instr(message,'private command results')",[],|r|r.get(0))?;
    assert_eq!(leak, 0);
    let buffered:String=sql.query_row("SELECT json_object('cursor',cursor,'type',kind,'actor',actor,'created_at',created_at,'payload',json(payload)) FROM events WHERE cursor=?",[dm.event_cursor],|r|r.get(0))?;
    assert!(
        b.recheck_event(serde_json::from_str(&buffered)?)?.is_none(),
        "SSE recheck exposed a private event"
    );
    assert!(matches!(
        b.swarm_poll_wait("ada/verify", "editor", "verify", cursor)?,
        WaitPoll::Pending
    ));
    Ok(())
}

#[test]
fn worker_permissions_rechecked_and_inspection_does_not_advance_a_waiter() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("private.sqlite");
    let owner = Board::open(&path)?;
    let run = start(&owner, "ada/chat", 8)?;
    owner.swarm_register_worker("ada", &run.slug, &spec("one"))?;
    owner.swarm_register_worker("ada/chat", &run.slug, &spec("two"))?;
    owner.swarm_register_worker("ada", &run.slug, &spec("three"))?;
    let grant = owner.set_grant("ada", None, "ada/three", &Rules::parse("editor=read,post")?)?;
    let token = owner
        .issue_credential("ada", "ada/three", None, None)?
        .token;
    let mut sibling = Board::open(&path)?;
    sibling.bind_token(&token)?;
    let dm = owner.swarm_dm("ada/one", "editor", "two", "not for three")?;
    let at = owner.cursor()?;
    assert_eq!(
        classify(
            &sibling
                .swarm_register_worker("ada/three", "editor", &spec("four"))
                .unwrap_err()
        )
        .0,
        Kind::Forbidden
    );
    assert_eq!(
        classify(&sibling.swarm_stop("ada/three", "editor").unwrap_err()).0,
        Kind::Forbidden
    );
    assert!(
        sibling
            .swarm_poll_wait("ada/three", "editor", "two", 0)
            .is_err()
    );
    assert!(
        sibling
            .swarm_read("ada/chat", "editor", 0, 50, true)
            .is_err()
    );
    assert!(
        sibling
            .swarm_latest("ada/three", "editor", None, 20, true)?
            .entries
            .iter()
            .all(|e| e.dm.is_none())
    );
    assert_eq!(
        sibling.swarm_status("ada/three", "editor")?.activity_count,
        4
    );
    assert!(
        sibling
            .event_feed(0, 500, &Default::default(), None, true)?
            .1
            .iter()
            .all(|e| e.cursor != dm.event_cursor)
    );
    assert_eq!(
        owner.cursor()?,
        at,
        "reads or rejected operations wrote an event"
    );
    owner.set_grant(
        "ada",
        Some(grant.id),
        "ada/three",
        &Rules::parse("editor=none")?,
    )?;
    assert_eq!(sibling.swarm_active_count("ada/three")?, 0);
    assert!(sibling.swarm_summaries("ada/three", None, 20)?.is_empty());
    assert!(
        sibling
            .swarm_read("ada/three", "editor", 0, 20, true)
            .is_err()
    );
    assert!(
        sibling
            .swarm_dm("ada/three", "editor", "two", "should fail")
            .is_err()
    );
    Ok(())
}

#[test]
fn capped_concurrent_notices_cannot_overspend_and_a_stale_waiter_cannot_advance() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("work.db");
    let b = Board::open(&path)?;
    start(&b, "ada", 1)?;
    add(&b, "impl")?;
    add(&b, "verify")?;
    let (_, last) = b.swarm_wait_target("ada/verify", "editor")?;
    let gate = Arc::new(Barrier::new(2));
    let go = |name: &'static str| {
        let g = gate.clone();
        let path = path.clone();
        std::thread::spawn(move || -> Result<_> {
            let db = Board::open(path)?;
            g.wait();
            db.swarm_post(
                "ada/impl",
                "editor",
                &format!("@verify {name}"),
                &Default::default(),
            )
        })
    };
    let a = go("one");
    let c = go("two");
    let results = [a.join().unwrap()?, c.join().unwrap()?];
    assert_eq!(results.iter().map(|r| r.notified.len()).sum::<usize>(), 1);
    assert_eq!(results.iter().map(|r| r.suppressed.len()).sum::<usize>(), 1);
    let WaitPoll::Ready { cursor, .. } =
        b.swarm_poll_wait("ada/verify", "editor", "verify", last)?
    else {
        panic!("expected notice")
    };
    let w = &b.swarm_status("ada", "editor")?.workers[1];
    assert_eq!(w.last_read_cursor, last);
    assert_eq!(b.swarm_wait_target("ada/verify", "editor")?.1, last);
    assert!(
        b.swarm_dm("ada/impl", "editor", "verify", "more work")
            .is_err()
    );
    b.swarm_dm("ada/impl", "editor", "coordinator", "raise the limit")?;
    assert!(matches!(
        b.swarm_poll_wait("ada", "editor", "coordinator", 0)?,
        WaitPoll::Ready { .. }
    ));
    b.swarm_set_limit("ada", "editor", 3)?;
    b.swarm_dm("ada/impl", "editor", "verify", "resume")?;
    b.swarm_done("ada/impl", "editor", Some("finished"))?;
    let done = b.swarm_stop("ada", "editor")?;
    assert_eq!(done.workers[0].status, "done");
    assert_eq!(done.workers[1].status, "stopped");
    assert!(matches!(
        b.swarm_poll_wait("ada/verify", "editor", "verify", cursor)?,
        WaitPoll::Stopped { .. }
    ));
    Ok(())
}

#[test]
fn role_and_skill_bytes_are_pinned_even_after_revision_or_deletion() -> Result<()> {
    let d = tempfile::tempdir()?;
    let db = d.path().join("saved.db");
    let b = Board::open(&db)?;
    let skill = d.path().join("offline-check");
    fs::create_dir(&skill)?;
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: offline-check\ndescription: Test offline\n---\nDo a cold start.",
    )?;
    let writer = b.role_library_as("ada");
    let spec_skill = writer.skill_import(&skill)?;
    let role = writer.role_create("qa", "QA", "Scoped checks", "Before prompt edits")?;
    writer.role_attach_skill(&role.slug, &spec_skill.info.slug)?;
    start(&b, "ada", 50)?;
    let args = SwarmAddWorker {
        role_slug: Some(role.slug.clone()),
        ..spec("2_api")
    };
    let spawn = b.swarm_register_worker("ada", "editor", &args)?;
    let binding = b.worker_binding(&spawn.selector)?;
    assert_eq!(
        b.worker_assignment(&binding)?.role_prompt,
        "Before prompt edits"
    );
    let stored = b
        .worker_skill_file(&binding, "offline-check", "SKILL.md")?
        .text;
    assert!(stored.contains("Do a cold start."));
    assert!(!d.path().join("saved.db.data").exists());
    writer.role_edit("qa", "qa", "QA", "changed", "Do not load old content", None)?;
    writer.delete_skill_bundle(&spec_skill.info.slug, true)?;
    writer.role_delete_if("qa", None)?;
    drop(b);
    let b = Board::open(db)?;
    let before = b.cursor()?;
    let retried = b.swarm_register_worker("ada", "editor", &args)?;
    assert_eq!(b.cursor()?, before);
    assert_eq!(retried.cli_args, spawn.cli_args);
    assert_eq!(
        b.worker_assignment(&binding)?.role_prompt,
        "Before prompt edits"
    );
    assert_eq!(
        b.worker_skill_file(&binding, "offline-check", "SKILL.md")?
            .text,
        stored
    );
    let msg = b.swarm_post("ada", "editor", "@2_api please rerun", &Default::default())?;
    assert_eq!(msg.notified, ["2_api"]);
    Ok(())
}

#[test]
fn inbox_waits_require_post_before_and_after_first_contact() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("inbox.db");
    let owner = Board::open(&path)?;
    start(&owner, "ada", 20)?;
    add(&owner, "verify")?;
    let actor = "ada/verify";
    owner.set_grant("ada", None, actor, &Rules::parse("editor=read")?)?;
    let mut reader = Board::open(&path)?;
    reader.bind_token(&owner.issue_credential("ada", actor, None, None)?.token)?;
    for checked_in in [false, true] {
        if checked_in {
            owner.swarm_check_in(actor, "editor", None, None, None)?;
        }
        let mut wait = reader.swarm_inbox_wait(actor, "editor", None, None)?;
        assert!(matches!(
            wait.try_poll(true)?,
            Some(SwarmWaitOutcome::StillWaiting { .. })
        ));
        assert_eq!(
            classify(&wait.try_poll(false).unwrap_err()).0,
            Kind::Forbidden
        );
    }
    owner.swarm_dm("ada", "editor", "verify", "review")?;
    let mut wait = reader.swarm_inbox_wait(actor, "editor", None, None)?;
    assert_eq!(
        classify(&wait.try_poll(true).unwrap_err()).0,
        Kind::Forbidden
    );
    Ok(())
}

#[test]
fn checked_in_participants_can_park_and_drop_waits_without_a_sqlite_writer() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("wait.db");
    let b = Board::open(&path)?;
    start(&b, "ada", 10)?;
    add(&b, "verify")?;
    b.swarm_check_in("ada/verify", "editor", None, None, None)?;
    let before = b.cursor()?;
    let other = rusqlite::Connection::open(&path)?;
    other.execute_batch("BEGIN IMMEDIATE")?;
    for actor in ["ada", "ada/verify"] {
        let mut wait = b.swarm_inbox_wait(actor, "editor", None, None)?;
        assert!(wait.try_poll(false)?.is_none());
        assert!(matches!(
            wait.try_poll(true)?,
            Some(SwarmWaitOutcome::StillWaiting { .. })
        ));
    }
    assert_eq!(b.cursor()?, before);
    Ok(())
}

#[test]
fn a_failed_notification_rolls_back_the_message_its_event_and_the_budget() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("rollback.sqlite");
    let b = Board::open(&path)?;
    start(&b, "ada", 20)?;
    add(&b, "impl")?;
    add(&b, "verify")?;
    let conn = rusqlite::Connection::open(&path)?;
    conn.execute_batch("CREATE TRIGGER simulate_failure BEFORE INSERT ON swarm_notifications BEGIN SELECT RAISE(ABORT,'failed to record wake'); END;")?;
    let head = b.cursor()?;
    assert!(
        b.swarm_post(
            "ada/impl",
            "editor",
            "@verify test this",
            &Default::default()
        )
        .is_err()
    );
    assert!(
        b.swarm_dm("ada/impl", "editor", "verify", "some private text")
            .is_err()
    );
    assert_eq!(b.cursor()?, head);
    let status = b.swarm_status("ada", "editor")?;
    assert_eq!(status.notifications_sent, 0);
    assert_eq!(status.activity_count, 3);
    assert_eq!(
        conn.query_row("SELECT count(*) FROM swarm_dms", [], |r| r.get::<_, i64>(0))?,
        0
    );
    Ok(())
}

#[test]
fn worker_slug_mentions_take_precedence_and_ignore_code_and_addresses_inside_markup() -> Result<()>
{
    let b = Board::memory()?;
    start(&b, "ada", 15)?;
    add(&b, "ada")?;
    add(&b, "qa_")?;
    let quiet = b.swarm_post(
        "ada",
        "editor",
        "qa_ or literal \u{60}@QA_\u{60}, also @[@qa_]",
        &Default::default(),
    )?;
    assert!(quiet.notified.is_empty());
    let active = b.swarm_post(
        "ada",
        "editor",
        "@ada please ask @QA_.",
        &Default::default(),
    )?;
    assert_eq!(active.notified, ["ada", "qa_"]);
    assert_eq!(active.message.text, "@[ada/ada] please ask @[ada/qa_].");
    Ok(())
}

#[test]
fn addressed_inbox_only_advances_when_its_owner_acknowledges_a_batch() -> Result<()> {
    let b = Board::memory()?;
    start(&b, "ada", 20)?;
    add(&b, "impl")?;
    add(&b, "verify")?;
    b.swarm_check_in("ada/verify", "editor", None, None, None)?;
    let dm = b.swarm_dm("ada/impl", "editor", "verify", "ready before wait")?;
    let before = b.cursor()?;
    let last = b.swarm_wait_target("ada/verify", "editor")?.1;
    let wait = |actor, after, ack| {
        b.swarm_wait(
            actor,
            "editor",
            after,
            ack,
            std::time::Duration::ZERO,
            || Ok(()),
        )
    };
    let outcome = wait("ada/verify", None, None)?;
    let SwarmWaitOutcome::Messages { cursor, dms, .. } = outcome else {
        panic!("expected queued DM immediately");
    };
    assert_eq!(cursor, dm.event_cursor);
    assert_eq!(dms.len(), 1);
    assert_eq!(
        b.cursor()?,
        before,
        "ready wait for an already-working worker should not emit waiting/working status churn"
    );
    assert_eq!(b.swarm_wait_target("ada/verify", "editor")?.1, last);
    assert!(matches!(
        wait("ada/verify", Some(cursor), None)?,
        SwarmWaitOutcome::StillWaiting { .. }
    ));
    assert!(matches!(
        wait("ada/verify", None, None)?,
        SwarmWaitOutcome::Messages { .. }
    ));
    assert!(
        wait("ada/impl", None, Some(cursor)).is_err(),
        "a DM sender cannot ack the recipient's inbox"
    );
    assert!(wait("ada/verify", Some(cursor), Some(cursor)).is_err());
    let next = b
        .swarm_dm("ada/impl", "editor", "verify", "second batch")?
        .event_cursor;
    for _ in 0..2 {
        assert!(
            matches!(wait("ada/verify", None, Some(cursor))?, SwarmWaitOutcome::Messages { cursor: c, .. } if c == next)
        );
    }
    assert!(
        matches!(wait("ada/verify", None, Some(next))?, SwarmWaitOutcome::StillWaiting {cursor: c} if c == next)
    );
    assert!(
        matches!(wait("ada/verify", None, Some(cursor))?, SwarmWaitOutcome::StillWaiting {cursor: c} if c == next),
        "an old ack must not rewind the saved cursor"
    );
    assert_eq!(b.swarm_wait_target("ada/verify", "editor")?.1, next);
    Ok(())
}
