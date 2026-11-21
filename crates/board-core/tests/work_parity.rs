use anyhow::Result;
use board_local::{Board, Filter, PostOptions, access::Rules, automation::*, errors::classify};
use serde_json::json;

fn new<'a>(channel: &'a str, text: &'a str, options: &'a PostOptions) -> TaskTarget<'a> {
    TaskTarget::New {
        channel,
        text,
        options,
    }
}
fn due() -> Timing {
    Timing {
        due: chrono::Utc::now().timestamp(),
        every: None,
        times: None,
        until: None,
    }
}
fn lease(b: &Board, id: i64) -> Result<Task> {
    claim(b, "worker", ClaimTarget::Id(id), 30, &Default::default())
}
fn grant(b: &Board, actor: &str, rules: &str) -> Result<String> {
    b.set_grant("root", None, actor, &Rules::parse(rules)?)?;
    Ok(b.issue_credential("root", actor, None, None)?.token)
}

#[test]
fn blind_task_and_plan_retries_return_creation_receipts_not_ongoing_work() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("blind.db");
    let b = Board::open(&path)?;
    let token = grant(&b, "ada/intake", "intake=post")?;
    let mut limited = Board::open(&path)?;
    limited.bind_token(&token)?;
    let opt = PostOptions {
        key: Some("blind-one".into()),
        no_sync: true,
        ..Default::default()
    };
    let t = create_task(
        &limited,
        "ada/intake",
        new("intake", "filed", &opt),
        &[],
        None,
    )?;
    assert_eq!((&t.owner, &t.by), (&"ada".into(), &"ada/intake".into()));
    assert!(b.get(t.id)?.no_sync);
    lease(&b, t.id)?;
    b.reply(t.id, "worker", "private discussion", &Default::default())?;
    assert!(get_task(&limited, t.id).is_err());
    assert!(limited.get(t.id).is_err());
    assert_eq!(
        create_task(
            &limited,
            "ada/intake",
            new("intake", "different", &opt),
            &[],
            None
        )?,
        t
    );
    let items = serde_json::from_value::<Vec<PlanItem>>(
        json!([{"key":"a","text":"intake","noSync":true},{"text":"second","needs":[{"key":"a"}]}]),
    )?;
    let batch = create_plan(&limited, "ada/intake", "intake", &items, Some("plan"))?;
    assert_eq!(batch[1].needs, [batch[0].id]);
    assert_eq!(batch[1].state, "waiting");
    lease(&b, batch[0].id)?;
    let mut limited = Board::open(&path)?;
    limited.bind_token(&token)?;
    assert_eq!(
        create_plan(&limited, "ada/intake", "intake", &items, Some("plan"))?,
        batch
    );
    assert_eq!(lookup_plan(&limited, "ada/intake", "plan")?, batch);
    assert!(b.get(batch[0].id)?.no_sync);
    assert!(!b.get(batch[1].id)?.no_sync);
    assert!(
        create_task(
            &limited,
            "ada/intake",
            new("intake", "guess", &Default::default()),
            &[t.id],
            None
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn principal_profile_and_post_only_owner_rights_are_distinct_from_claim_permission() -> Result<()> {
    let d = tempfile::tempdir()?;
    let path = d.path().join("own.db");
    let b = Board::open(&path)?;
    let token = grant(&b, "ada/build", "work=read,post")?;
    let mut build = Board::open(&path)?;
    build.bind_token(&token)?;
    let t = create_task(
        &build,
        "ada/build",
        new("work", "build", &Default::default()),
        &[],
        None,
    )?;
    assert_eq!(
        change_task(
            &build,
            t.id,
            "ada/build",
            None,
            None,
            TaskAction::Block("approval")
        )?
        .state,
        "blocked"
    );
    assert_eq!(
        change_task(&build, t.id, "ada/build", None, None, TaskAction::Unblock)?.state,
        "ready"
    );
    assert!(
        claim(
            &build,
            "ada/build",
            ClaimTarget::Id(t.id),
            30,
            &Default::default()
        )
        .is_err()
    );
    assert!(
        change_task(
            &b,
            t.id,
            "ada/test",
            None,
            None,
            TaskAction::Cancel("sibling")
        )
        .is_err()
    );
    assert_eq!(
        change_task(&b, t.id, "ada", None, None, TaskAction::Cancel("owner"))?.state,
        "canceled"
    );
    let root = b.post("work", "ada/build", "before", &Default::default())?;
    let promoted = create_task(&b, "ada", TaskTarget::Existing(root.id), &[], None)?;
    assert_eq!(promoted.by, "ada");
    b.edit(root.id, "ada/build", "correction", None)?;
    assert_eq!(get_task(&b, root.id)?.hold_kind.as_deref(), Some("edited"));
    Ok(())
}

#[test]
fn newest_tasks_are_selected_by_reply_activity_before_limit_and_report_live_reply_summary()
-> Result<()> {
    let b = Board::memory()?;
    let opts = PostOptions::default();
    let jobs: Vec<_> = (0..3)
        .map(|i| create_task(&b, "ada", new("ops", &format!("task{i}"), &opts), &[], None))
        .collect::<Result<_>>()?;
    for i in [1, 2, 0] {
        b.reply(jobs[i].id, "worker", "progress", &opts)?;
    }
    let removed = b.reply(jobs[1].id, "worker", "scratch", &opts)?;
    b.delete(removed.id, "worker", None)?;
    let (head, rows) = filtered_tasks_page(
        &b,
        &TaskFilter {
            latest: true,
            ..Default::default()
        },
        0,
        2,
    )?;
    assert_eq!(head, b.cursor()?);
    assert_eq!(
        rows.iter().map(|t| t.id).collect::<Vec<_>>(),
        [jobs[2].id, jobs[0].id]
    );
    assert_eq!(rows[1].replies, Some(1));
    assert_eq!(rows[1].last.as_ref().unwrap().text, "progress");
    assert!(
        filtered_tasks_page(
            &b,
            &TaskFilter {
                latest: true,
                ..Default::default()
            },
            1,
            2
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn needs_checks_only_added_edges_prunes_visible_completed_work_and_limits_direct_edges()
-> Result<()> {
    let d = tempfile::tempdir()?;
    let path = d.path().join("needs.db");
    let b = Board::open(&path)?;
    let opts = Default::default();
    // Build a real 501-deep graph; a new task cannot already have dependents and doesn't walk it.
    let mut line = Vec::new();
    for _ in 0..501 {
        let needs = line.last().copied().into_iter().collect::<Vec<_>>();
        line.push(create_task(&b, "ada", new("work", "step", &opts), &needs, None)?.id);
    }
    let root = create_task(&b, "ada", new("work", "root", &opts), &[], None)?;
    assert_eq!(
        classify(
            &change_task(
                &b,
                root.id,
                "ada",
                None,
                None,
                TaskAction::Needs(&line[500..])
            )
            .unwrap_err()
        )
        .1,
        "invalid_needs"
    );
    change_task(
        &b,
        root.id,
        "ada",
        None,
        None,
        TaskAction::Needs(&line[499..500]),
    )?;
    assert!(
        create_task(
            &b,
            "ada",
            new("work", "twenty-one", &opts),
            &line[..21],
            None
        )
        .is_err()
    );
    let both = create_task(
        &b,
        "ada",
        new("work", "drop only", &opts),
        &[line[500], root.id],
        None,
    )?;
    change_task(
        &b,
        both.id,
        "ada",
        None,
        None,
        TaskAction::Needs(&line[500..]),
    )?;
    for id in line.iter().copied() {
        let t = lease(&b, id)?;
        change_task(
            &b,
            id,
            "worker",
            Some(t.epoch),
            t.claim_token.as_deref(),
            TaskAction::Complete(None),
        )?;
    }
    assert_eq!(
        change_task(
            &b,
            root.id,
            "ada",
            None,
            None,
            TaskAction::Needs(&line[500..])
        )?
        .state,
        "ready"
    );
    let hidden: Vec<_> = (0..5)
        .map(|_| create_task(&b, "ada", new("private", "private", &opts), &[], None).map(|t| t.id))
        .collect::<Result<_>>()?;
    let mut next = create_task(&b, "ada", new("private", "hidden", &opts), &hidden, None)?.id;
    let hidden_parent = next;
    for _ in 0..495 {
        next = create_task(&b, "ada", new("work", "edge", &opts), &[next], None)?.id;
    }
    let token = grant(&b, "ada", "work=read,post,label")?;
    let mut limited = Board::open(&path)?;
    limited.bind_token(&token)?;
    let attempt = || {
        change_task(
            &limited,
            root.id,
            "ada",
            None,
            None,
            TaskAction::Needs(&[next]),
        )
        .unwrap_err()
        .to_string()
    };
    let before = attempt();
    for id in hidden.into_iter().chain([hidden_parent]) {
        let t = lease(&b, id)?;
        change_task(
            &b,
            id,
            "worker",
            Some(t.epoch),
            t.claim_token.as_deref(),
            TaskAction::Complete(None),
        )?;
    }
    assert_eq!(attempt(), before);
    assert!(before.contains("500"));
    change_task(&b, root.id, "ada", None, None, TaskAction::Needs(&[next]))?;
    Ok(())
}

fn spec(name: &str, channel: &str) -> WatchSpec {
    WatchSpec {
        name: name.into(),
        filter: Filter {
            channel: Some(channel.into()),
            ..Default::default()
        },
        kind: Some("message.posted".into()),
        all: false,
    }
}
#[test]
fn watcher_set_replacement_preserves_leases_and_checkpoints_and_temporary_caps_cannot_pull_or_ack()
-> Result<()> {
    let d = tempfile::tempdir()?;
    let path = d.path().join("set.db");
    let mut b = Board::open(&path)?;
    let token = grant(&b, "ada/w", "ops=read;build=read")?;
    let mut reg = Board::open(&path)?;
    reg.bind_token(&token)?;
    let set = [spec("ops", "ops"), spec("build", "build")];
    replace_watchers(&reg, "ada/w", &set)?;
    let one = b.post("ops", "ada", "old", &Default::default())?;
    b.post("build", "ada", "two", &Default::default())?;
    let supplied = lease_matches(&reg, 10, 60)?;
    assert_eq!(supplied.len(), 2);
    let d = supplied.iter().find(|d| d.event.cursor == one.id).unwrap();
    let head = b.cursor()?;
    replace_watchers(&reg, "ada/w", &set)?;
    assert_eq!(head, b.cursor()?);
    b.with_request(Some(&token), "root", Some("ops=read"), |c, actor| {
        assert_eq!(
            classify(&replace_watchers(c, actor, &[]).unwrap_err()).1,
            "scope_not_durable"
        );
        assert_eq!(
            classify(&lease_matches(c, 1, 30).unwrap_err()).1,
            "scope_not_durable"
        );
        assert_eq!(
            classify(&next_event(c, "ops").unwrap_err()).1,
            "scope_not_durable"
        );
        assert_eq!(
            classify(&ack_delivery(c, "ops", one.id, Some(&d.receipt)).unwrap_err()).1,
            "scope_not_durable"
        );
        Ok(())
    })?;
    assert!(
        replace_watchers(
            &reg,
            "ada/w",
            &[spec("duplicate", "ops"), spec("duplicate", "ops")]
        )
        .is_err()
    );
    let old = b.post("ops", "ada", "before replace", &Default::default())?;
    replace_watchers(&reg, "ada/w", &[spec("ops", "build")])?;
    ack_delivery(&reg, "ops", one.id, Some(&d.receipt))?;
    assert_eq!(next_event(&reg, "ops")?.unwrap().cursor, old.id);
    ack_event(&reg, "ops", old.id)?;
    b.post("ops", "ada", "no new match", &Default::default())?;
    let id = b.post("build", "ada", "new", &Default::default())?.id;
    drop(reg);
    let mut reg = Board::open(path)?;
    reg.bind_token(&token)?;
    assert_eq!(next_event(&reg, "ops")?.unwrap().cursor, id);
    Ok(())
}

#[test]
fn scheduled_text_and_sync_policy_are_fixed_at_registration_and_dropped_send_does_not_stall_queue()
-> Result<()> {
    let b = Board::memory()?;
    b.post("ops", "ada", "here", &Default::default())?;
    let s = schedule_post(
        &b,
        "ada",
        Destination::Channel("ops"),
        "notify @nobody and @ada",
        due(),
        &PostOptions {
            no_sync: true,
            ..Default::default()
        },
    )?;
    assert!(
        s.mentions
            .iter()
            .any(|m| m.written == "@nobody" && m.actor.is_none())
    );
    assert!(s.text.contains("@[ada]"));
    let reject = schedule(
        &b,
        "ada",
        Destination::Channel("ops"),
        "REJECT",
        due(),
        Default::default(),
    )?;
    b.set_write_rule(
        "owner",
        &board_local::policy::Rule {
            name: "skip".into(),
            priority: 0,
            script: r#"if write.text == "REJECT" { #{drop: "example reason"} } else { #{} }"#
                .into(),
        },
    )?;
    b.post("ops", "nobody", "here later", &Default::default())?;
    assert_eq!(dispatch_due(&b)?, 1);
    assert_eq!(dispatch_due(&b)?, 0);
    let rejected = get_schedule(&b, reject.id)?;
    assert_eq!(rejected.state, "dropped");
    assert_eq!(rejected.sent, 0);
    assert_eq!(rejected.canceled.unwrap().reason, "dropped");
    assert!(rejected.next.is_none() && rejected.last.is_none());
    let posted = b
        .events(0, 100)?
        .into_iter()
        .filter_map(|e| e.message)
        .find(|m| m.schedule.as_ref().is_some_and(|s2| s2.id == s.id))
        .unwrap();
    assert_eq!(posted.text, "notify @nobody and @[ada]");
    assert!(posted.no_sync);
    b.create_source(
        "ada",
        "local-copy",
        board_local::sources::Provider::Local,
        "example",
        "ops",
    )?;
    b.set_outbound("ada", "local-copy", true)?;
    assert!(
        b.start_source_thread("ada", "local-copy", posted.id)
            .is_err()
    );
    assert!(
        b.inbox(
            "ada",
            "local-copy",
            board_local::sources::Direction::Out,
            0,
            10
        )?
        .is_empty()
    );
    let allowed = b.post("ops", "ada", "actually share", &Default::default())?;
    b.start_source_thread("ada", "local-copy", allowed.id)?;
    assert_eq!(
        b.inbox(
            "ada",
            "local-copy",
            board_local::sources::Direction::Out,
            0,
            10
        )?
        .len(),
        1
    );
    assert_eq!(get_schedule(&b, s.id)?.last, Some(posted.id));
    assert_eq!(s.first, s.next);
    Ok(())
}

#[test]
fn change_feed_and_watchers_include_both_sides_of_readiness_and_only_verbose_watches_see_renewal()
-> Result<()> {
    let b = Board::memory()?;
    let opts = Default::default();
    let dep = create_task(&b, "ada", new("ops", "dep", &opts), &[], None)?;
    let t = lease(&b, dep.id)?;
    change_task(
        &b,
        dep.id,
        "worker",
        Some(t.epoch),
        t.claim_token.as_deref(),
        TaskAction::Complete(None),
    )?;
    let jobs = |name: &str, all| {
        add_watcher_spec(
            &b,
            "ada",
            WatchSpec {
                name: name.into(),
                filter: Filter {
                    channel: Some("ops".into()),
                    ..Default::default()
                },
                kind: None,
                all,
            },
        )
    };
    jobs("normal", false)?;
    jobs("verbose", true)?;
    let t = create_task(&b, "ada", new("ops", "new", &opts), &[dep.id], None)?;
    change_task(&b, t.id, "ada", None, None, TaskAction::Block("wait"))?;
    change_task(&b, t.id, "ada", None, None, TaskAction::Unblock)?;
    let held = lease(&b, t.id)?;
    change_task(
        &b,
        t.id,
        "worker",
        Some(held.epoch),
        held.claim_token.as_deref(),
        TaskAction::Renew(60),
    )?;
    let f = Filter {
        labels: std::collections::BTreeMap::from([("board.task".into(), "ready".into())]),
        ..Default::default()
    };
    let events = b.events(0, 200)?;
    let selected: Vec<_> = events
        .iter()
        .filter(|e| e.message.as_ref().map(|m| m.id) == Some(t.id) && e.matches(&f))
        .map(|e| e.kind.as_str())
        .collect();
    assert_eq!(
        selected,
        ["task.ready", "task.blocked", "task.ready", "task.claimed"]
    );
    for (name, expect) in [("normal", false), ("verbose", true)] {
        let mut renew = false;
        while let Some(e) = next_event(&b, name)? {
            renew |= e.kind == "task.renewed";
            ack_event(&b, name, e.cursor)?;
        }
        assert_eq!(renew, expect);
    }
    Ok(())
}

#[test]
fn imported_edits_hold_adopted_task_and_cancelled_prerequisites_are_rechecked_on_noops()
-> Result<()> {
    use board_local::sources::*;
    let b = Board::memory()?;
    b.create_source("ada", "reports", Provider::Local, "team", "work")?;
    let envelope = |key: &str, change: Change| Envelope {
        key: key.into(),
        import: Import {
            links: Vec::new(),
            thread: "42".into(),
            external: "origin".into(),
            author: "42".into(),
            profile: None,
            parent: None,
            url: None,
            updated_at: None,
            no_sync: true,
            mentioned: Default::default(),
            change,
        },
        raw: json!({}),
    };
    let m = b.import_source(
        "ada",
        "reports",
        &envelope(
            "once",
            Change::Upsert {
                text: "original".into(),
                labels: Default::default(),
            },
        ),
    )?;
    let t = create_task(&b, "ada", TaskTarget::Existing(m.id), &[], None)?;
    let claimed = lease(&b, t.id)?;
    b.import_source(
        "ada",
        "reports",
        &envelope(
            "twice",
            Change::Edit {
                text: "different request".into(),
            },
        ),
    )?;
    assert_eq!(get_task(&b, t.id)?.hold_kind.as_deref(), Some("edited"));
    assert!(
        change_task(
            &b,
            t.id,
            "worker",
            Some(claimed.epoch),
            claimed.claim_token.as_deref(),
            TaskAction::Complete(None)
        )
        .is_err()
    );
    let dep = create_task(
        &b,
        "ada",
        new("work", "dependent", &Default::default()),
        &[t.id, t.id],
        None,
    )?;
    assert_eq!(dep.needs, [t.id]);
    change_task(&b, t.id, "ada", None, None, TaskAction::Cancel("closed"))?;
    assert_eq!(
        classify(
            &change_task(&b, dep.id, "ada", None, None, TaskAction::Needs(&[t.id])).unwrap_err()
        )
        .1,
        "invalid_needs"
    );
    Ok(())
}

#[test]
fn canceled_wait_before_commit_rolls_back_queue_claim_and_proof() -> Result<()> {
    let b = Board::memory()?;
    let t = create_task(
        &b,
        "ada",
        new("work", "next", &Default::default()),
        &[],
        None,
    )?;
    let head = b.cursor()?;
    let checked = std::cell::Cell::new(false);
    let proof = "private-before-abort";
    assert!(
        claim_while(
            &b,
            "ada",
            ClaimTarget::Ready("work"),
            ClaimRequest {
                lease_seconds: 30,
                token: proof
            },
            &Default::default(),
            &[],
            || !checked.replace(true)
        )
        .is_err()
    );
    assert_eq!(b.cursor()?, head);
    assert_eq!(get_task(&b, t.id)?.epoch, 0);
    assert!(recover_claim(&b, "ada", proof).is_err());
    Ok(())
}

#[test]
fn queue_poll_only_expires_the_overdue_lease_it_can_actually_take() -> Result<()> {
    let d = tempfile::tempdir()?;
    let path = d.path().join("queue.db");
    let b = Board::open(&path)?;
    let opts = Default::default();
    let foreign = create_task(&b, "bob", new("ops", "unrelated", &opts), &[], None)?;
    let own = create_task(&b, "ada", new("ops", "mine", &opts), &[], None)?;
    for t in [&foreign, &own] {
        lease(&b, t.id)?;
    }
    let raw = rusqlite::Connection::open(&path)?;
    raw.execute("UPDATE tasks SET lease_until=0", [])?;
    let root = claim(
        &b,
        "ada",
        ClaimTarget::Ready("ops"),
        30,
        &std::collections::BTreeMap::from([("board.task".into(), "ready".into())]),
    )?;
    assert_eq!(root.id, own.id);
    assert_eq!(root.epoch, 2);
    assert_eq!(
        raw.query_row("SELECT state FROM tasks WHERE id=?", [foreign.id], |r| {
            r.get::<_, String>(0)
        })?,
        "claimed"
    );
    assert!(
        claim(
            &b,
            "ada",
            ClaimTarget::Ready("ops"),
            30,
            &Default::default()
        )
        .is_err()
    );
    assert_eq!(
        raw.query_row("SELECT state FROM tasks WHERE id=?", [foreign.id], |r| {
            r.get::<_, String>(0)
        })?,
        "claimed"
    );
    Ok(())
}

#[test]
fn legacy_profile_task_snapshots_retain_the_original_creator_without_new_events() -> Result<()> {
    let d = tempfile::tempdir()?;
    let path = d.path().join("legacy.db");
    let b = Board::open(&path)?;
    let id = create_task(
        &b,
        "ada/triage",
        new("intake", "review", &Default::default()),
        &[],
        None,
    )?
    .id;
    let head = b.cursor()?;
    drop(b);
    let raw = rusqlite::Connection::open(&path)?;
    raw.execute("UPDATE events SET message=json_remove(json_set(message,'$.task.owner','ada/triage'),'$.task.by'),payload=json_remove(json_set(payload,'$.task.owner','ada/triage'),'$.task.by') WHERE kind='task.created'",[])?;
    raw.pragma_update(None, "user_version", 7)?;
    drop(raw);
    let b = Board::open(path)?;
    assert_eq!(b.cursor()?, head);
    let ev = b
        .events(0, 30)?
        .into_iter()
        .find(|e| e.kind == "task.created")
        .unwrap();
    let t = ev.message.unwrap().task.unwrap();
    assert_eq!(
        (t.id, t.owner.as_str(), t.by.as_str()),
        (id, "ada", "ada/triage")
    );
    assert_eq!(ev.payload["task"]["by"], "ada/triage");
    Ok(())
}

#[test]
fn scheduling_a_saved_draft_is_atomic_with_retirement_including_failure_and_stale_windows()
-> Result<()> {
    use board_local::{ComposerSettings, Draft};
    let d = tempfile::tempdir()?;
    let path = d.path().join("composer.db");
    let b = Board::open(&path)?;
    let draft = Draft {
        body: "pending reminder".into(),
        channel: "ops".into(),
        reply_to: None,
    };
    let at = chrono::Utc::now().timestamp() + 600;
    let options = ComposerSettings {
        no_sync: true,
        later: true,
        labels: "phase=private".into(),
        due: chrono::DateTime::from_timestamp(at, 0)
            .unwrap()
            .to_rfc3339(),
        ..Default::default()
    };
    let send = PostOptions {
        no_sync: true,
        labels: [("phase".into(), "private".into())].into(),
        files: vec![("private.txt".into(), b"original".to_vec())],
        ..Default::default()
    };
    let stamp = b
        .save_composer(
            "ada",
            "new",
            None,
            Some((&draft, &options)),
            Some(&send.files),
        )?
        .unwrap();
    let raw = rusqlite::Connection::open(&path)?;
    let submit = |stamp: &str, opts: &PostOptions, due: i64| {
        schedule_draft(
            &b,
            "ada",
            Destination::Channel("ops"),
            &draft.body,
            Timing {
                due,
                every: None,
                times: None,
                until: None,
            },
            opts,
            ("new", stamp),
        )
    };
    let head = b.cursor()?;
    for changed in [
        PostOptions {
            no_sync: false,
            ..send.clone()
        },
        PostOptions {
            labels: Default::default(),
            ..send.clone()
        },
        PostOptions {
            files: vec![("private.txt".into(), b"replaced".to_vec())],
            ..send.clone()
        },
    ] {
        assert_eq!(
            classify(&submit(&stamp, &changed, at).unwrap_err()).1,
            "draft_changed"
        );
    }
    assert_eq!(
        classify(&submit(&stamp, &send, at + 60).unwrap_err()).1,
        "draft_changed"
    );
    raw.execute_batch("CREATE TRIGGER stop_draft_schedule BEFORE INSERT ON schedules BEGIN SELECT RAISE(ABORT,'fixture: schedule storage failed'); END;")?;
    assert!(
        format!("{:#}", submit(&stamp, &send, at).unwrap_err())
            .contains("fixture: schedule storage failed")
    );
    assert_eq!(b.cursor()?, head);
    assert_eq!(b.load_composer("ada", "new")?.unwrap().stamp, stamp);
    assert!(schedules(&b, true)?.is_empty());
    raw.execute_batch("DROP TRIGGER stop_draft_schedule")?;
    assert_eq!(
        classify(&submit("stale-window", &send, at).unwrap_err()).1,
        "draft_changed"
    );
    let s = submit(&stamp, &send, at)?;
    assert!(s.no_sync);
    assert_eq!(s.first, Some(at));
    assert_eq!(s.attachments[0].name, "private.txt");
    assert!(b.load_composer("ada", "new")?.is_none());
    assert!(submit(&stamp, &send, at).is_err());
    drop(b);
    let b = Board::open(&path)?;
    assert!(b.load_composer("ada", "new")?.is_none());
    assert_eq!(schedules(&b, true)?.len(), 1);
    assert_eq!(get_schedule(&b, s.id)?.text, "pending reminder");
    Ok(())
}
