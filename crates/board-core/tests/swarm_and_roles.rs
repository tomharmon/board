use anyhow::Result;
use board_local::{
    Board, PostOptions,
    access::Rules,
    automation::get_task,
    errors::{Kind, classify},
    swarm::*,
};
use std::{
    fs,
    path::Path,
    sync::{Arc, Barrier},
};

fn start(b: &Board, actor: &str, max: i64) -> Result<SwarmRun> {
    b.swarm_start(
        actor,
        &SwarmStart {
            goal: "Fix the offline source editor".into(),
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
fn add(b: &Board, name: &str) -> Result<SpawnedWorker> {
    b.swarm_add_worker("ada", "editor", &spec(name))
}

#[test]
fn local_owner_can_supervise_another_coordinator_without_taking_its_inbox() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("board.db");
    let owner = Board::open(&path)?;
    for actor in ["guest/chat", "guest/impl", "outsider"] {
        owner.set_grant(
            "ada",
            None,
            actor,
            &Rules::parse("swarms/editor=read,post")?,
        )?;
    }
    let delegated = |actor: &str| -> Result<Board> {
        let token = owner.issue_credential("ada", actor, None, None)?.token;
        let mut b = Board::open(&path)?;
        b.bind_token(&token)?;
        Ok(b)
    };
    let coordinator = delegated("guest/chat")?;
    start(&coordinator, "guest/chat", 5)?;
    coordinator.swarm_add_worker("guest/chat", "editor", &spec("impl"))?;
    let other = delegated("outsider")?;
    assert!(!other.swarm_status("outsider", "editor")?.can_control);
    assert!(owner.swarm_status("ada", "editor")?.can_control);
    assert!(other.swarm_stop("outsider", "editor").is_err());
    owner.swarm_add_worker("ada", "editor", &spec("verify"))?;
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
fn real_v14_fixture_upgrades_existing_data_and_retires_executable_tables() -> Result<()> {
    let d = tempfile::tempdir()?;
    let path = d.path().join("original.sqlite");
    let mut input =
        flate2::read::GzDecoder::new(&include_bytes!("fixtures/swarm-v14.sqlite3.gz")[..]);
    std::io::copy(&mut input, &mut fs::File::create(&path)?)?;
    let check = rusqlite::Connection::open(&path)?;
    assert_eq!(
        check.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?,
        14
    );
    assert_eq!(
        check.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name IN ('agent_roles','swarm_runs')",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        0
    );
    assert_eq!(
        check.query_row("SELECT name FROM write_rules", [], |r| r
            .get::<_, String>(0))?,
        "old-local-rule"
    );
    let old = check.query_row("SELECT max(cursor) FROM events", [], |r| r.get::<_, i64>(0))?;
    drop(check);
    let b = Board::open(&path)?;
    assert_eq!(b.cursor()?, old);
    assert_eq!(b.get(1)?.text, "Persist this v14 message");
    assert_eq!(get_task(&b, 2)?.state, "ready");
    assert_eq!(b.roles_list()?.len(), 6);
    let check = rusqlite::Connection::open(&path)?;
    assert_eq!(
        check.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?,
        19
    );
    assert_eq!(check.query_row("SELECT count(*) FROM sqlite_schema WHERE name IN ('write_rules','rule_internal','sync_renderers','source_renderers','sync_render_labels')",[],|r|r.get::<_,i64>(0))?,0);
    assert_eq!(
        check.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?,
        "ok"
    );
    assert!(start(&b, "ada", 20)?.workers.is_empty());
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
    owner.swarm_add_worker("ada", &run.slug, &spec("one"))?;
    owner.swarm_add_worker("ada/chat", &run.slug, &spec("two"))?;
    owner.swarm_add_worker("ada", &run.slug, &spec("three"))?;
    let grant = owner.set_grant(
        "ada",
        None,
        "ada/three",
        &Rules::parse("swarms/editor=read,post")?,
    )?;
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
                .swarm_add_worker("ada/three", "editor", &spec("four"))
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
        &Rules::parse("swarms/editor=none")?,
    )?;
    assert_eq!(sibling.swarm_active_count("ada/three")?, 0);
    assert!(sibling.swarm_list("ada/three", None, 20)?.is_empty());
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
    b.swarm_set_worker_status("ada/verify", "editor", "verify", "waiting", Some(11))?;
    b.swarm_set_worker_status("ada/verify", "editor", "verify", "waiting", Some(22))?;
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
    b.swarm_leave_wait("ada/verify", "editor", "verify", 11, Some(cursor))?;
    let w = &b.swarm_status("ada", "editor")?.workers[1];
    assert_eq!(w.last_read_cursor, last);
    assert_eq!(w.waiter_pid, Some(22));
    b.swarm_leave_wait("ada/verify", "editor", "verify", 22, Some(cursor))?;
    assert_eq!(b.swarm_wait_target("ada/verify", "editor")?.1, cursor);
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
    let spec_skill = b.skill_import(&skill)?;
    let role = b.role_create("qa", "QA", "Scoped checks", "Before prompt edits")?;
    b.role_attach_skill(&role.slug, &spec_skill.info.slug)?;
    start(&b, "ada", 50)?;
    let args = SwarmAddWorker {
        role_slug: Some(role.slug.clone()),
        ..spec("2_api")
    };
    let spawn = b.swarm_add_worker("ada", "editor", &args)?;
    assert_eq!(spawn.env_vars["BOARD_PROFILE"], "2_api");
    assert_eq!(Path::new(&spawn.env_vars["BOARD_DB"]), db.canonicalize()?);
    assert!(
        spawn.assembled_prompt.contains("Before prompt edits")
            && spawn.assembled_prompt.contains("Do a cold start.")
    );
    let file = spawn.skills_dir.join("offline-check/SKILL.md");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&file)?.permissions().mode() & 0o077, 0);
    }
    let stored = fs::read(&file)?;
    b.role_edit("qa", "qa", "QA", "changed", "Do not load old content", None)?;
    b.delete_skill_bundle(&spec_skill.info.slug, true)?;
    b.role_delete("qa")?;
    drop(b);
    let b = Board::open(db)?;
    let before = b.cursor()?;
    let retried = b.swarm_add_worker("ada", "editor", &args)?;
    assert_eq!(b.cursor()?, before);
    assert_eq!(retried.assembled_prompt, spawn.assembled_prompt);
    assert_eq!(fs::read(file)?, stored);
    let msg = b.swarm_post("ada", "editor", "@2_api please rerun", &Default::default())?;
    assert_eq!(msg.notified, ["2_api"]);
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

#[cfg(unix)]
#[test]
fn materializing_a_role_rejects_existing_symlinks_without_publishing_a_worker() -> Result<()> {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir()?;
    let out = tempfile::tempdir()?;
    let path = dir.path().join("links.db");
    let b = Board::open(&path)?;
    start(&b, "ada", 20)?;
    let at = b.cursor()?;
    symlink(out.path(), dir.path().join("links.db.data"))?;
    assert!(
        b.swarm_add_worker(
            "ada",
            "editor",
            &SwarmAddWorker {
                role_slug: Some("implementer".into()),
                ..spec("builder")
            }
        )
        .is_err()
    );
    assert!(b.swarm_status("ada", "editor")?.workers.is_empty());
    assert_eq!(b.cursor()?, at);
    assert!(fs::read_dir(out.path())?.next().is_none());
    Ok(())
}

#[test]
fn ready_wait_advances_cursor_without_spurious_waiting_status_events() -> Result<()> {
    let b = Board::memory()?;
    start(&b, "ada", 20)?;
    add(&b, "impl")?;
    add(&b, "verify")?;
    b.swarm_check_in("ada/verify", "editor", "verify")?;
    let dm = b.swarm_dm("ada/impl", "editor", "verify", "ready before wait")?;
    let before = b.cursor()?;
    let outcome = b.swarm_wait(
        "ada/verify",
        "editor",
        None,
        std::time::Duration::from_secs(5),
        || Ok(()),
    )?;
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
    assert_eq!(
        b.swarm_wait_target("ada/verify", "editor")?.1,
        dm.event_cursor
    );
    Ok(())
}
