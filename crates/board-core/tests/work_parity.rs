use anyhow::Result;
use board_local::{Board, Filter, PostOptions, access::Rules, automation::*, errors::classify};
use serde_json::json;

#[test]
fn saved_task_submission_fences_private_routing_files_and_double_submits_after_reopen() -> Result<()>
{
    use board_local::{ComposerSettings, Draft, TaskDraftSettings};
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("task.db");
    let b = Board::open(&db)?;
    let blank = PostOptions::default();
    let prep = create_task(&b, "ana", new("ops", "Prepare", &blank), &[], None)?;
    let draft = Draft {
        body: "Review package".into(),
        channel: "ops".into(),
        reply_to: None,
    };
    let settings = ComposerSettings {
        task: Some(TaskDraftSettings {
            needs: format!("#{}", prep.id),
            assignee: "ana/build".into(),
        }),
        no_sync: true,
        ..Default::default()
    };
    let opts = PostOptions {
        no_sync: true,
        files: vec![("check.txt".into(), b"proof".to_vec())],
        ..Default::default()
    };
    let stamp = b
        .save_composer(
            "ana",
            "tasks",
            None,
            Some((&draft, &settings)),
            Some(&opts.files),
        )?
        .unwrap();
    let head = b.cursor()?;
    let submit = |c: &str, o: &PostOptions, needs: &[i64], assignee: Option<&str>, stamp: &str| {
        create_task_draft(&b, "ana", new(c, &draft.body, o), needs, assignee, stamp)
    };
    let wants = Some("ana/build");
    for refused in [
        submit("other", &opts, &[prep.id], wants, &stamp),
        submit("ops", &opts, &[], wants, &stamp),
        submit("ops", &opts, &[prep.id], None, &stamp),
        submit("ops", &opts, &[prep.id], wants, "stale"),
        submit(
            "ops",
            &PostOptions {
                no_sync: false,
                ..opts.clone()
            },
            &[prep.id],
            wants,
            &stamp,
        ),
        submit(
            "ops",
            &PostOptions {
                files: vec![("check.txt".into(), b"wrong".to_vec())],
                ..opts.clone()
            },
            &[prep.id],
            wants,
            &stamp,
        ),
    ] {
        assert_eq!(classify(&refused.unwrap_err()).1, "draft_changed");
    }
    let raw = rusqlite::Connection::open(&db)?;
    raw.execute_batch("CREATE TRIGGER fail_task_publish BEFORE INSERT ON tasks BEGIN SELECT RAISE(ABORT,'test commit failure'); END;")?;
    assert!(submit("ops", &opts, &[prep.id], wants, &stamp).is_err());
    assert_eq!(b.cursor()?, head);
    assert_eq!(b.load_composer("ana", "tasks")?.unwrap().stamp, stamp);
    raw.execute_batch("DROP TRIGGER fail_task_publish")?;
    let t = submit("ops", &opts, &[prep.id], wants, &stamp)?;
    assert_eq!(
        (t.state.as_str(), t.needs, t.assignee.as_deref()),
        ("waiting", vec![prep.id], wants)
    );
    let m = b.get(t.id)?;
    assert!(m.no_sync);
    assert_eq!(b.download(m.attachments[0].id)?.1, b"proof");
    drop(b);
    let b = Board::open(&db)?;
    assert!(b.load_composer("ana", "tasks")?.is_none());
    assert!(
        create_task_draft(
            &b,
            "ana",
            new("ops", &draft.body, &opts),
            &[prep.id],
            wants,
            &stamp
        )
        .is_err()
    );
    assert_eq!(list_tasks(&b, None, Some("all"))?.len(), 2);
    Ok(())
}

fn new<'a>(channel: &'a str, text: &'a str, options: &'a PostOptions) -> TaskTarget<'a> {
    TaskTarget::New {
        channel,
        text,
        options,
    }
}
fn lease(b: &Board, id: i64) -> Result<Task> {
    claim(b, "worker", id, 30)
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
        json!([{"key":"a","text":"intake","noSync":true},{"text":"second","needs":["a"]}]),
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
    assert!(claim(&build, "ada/build", t.id, 30).is_err());
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
fn local_only_posts_keep_their_settled_mentions_and_cannot_start_a_mirror() -> Result<()> {
    let b = Board::memory()?;
    b.post("ops", "ada", "here", &Default::default())?;
    let posted = b.post(
        "ops",
        "ada",
        "notify @nobody and @ada",
        &PostOptions {
            no_sync: true,
            ..Default::default()
        },
    )?;
    assert!(
        posted
            .mentions
            .iter()
            .any(|m| m.written == "@nobody" && m.actor.is_none())
    );
    b.post("ops", "nobody", "here later", &Default::default())?;
    let posted = b.get(posted.id)?;
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
    Ok(())
}

#[test]
fn task_state_is_recorded_at_event_time_and_only_verbose_watches_see_renewal() -> Result<()> {
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
        tasks_only: true,
        ..Default::default()
    };
    let events = b.events(0, 200)?;
    let selected: Vec<_> = events
        .iter()
        .filter(|e| {
            e.kind == "task.ready"
                && e.message.as_ref().is_some_and(|m| {
                    m.id == t.id && m.task.as_ref().is_some_and(|t| t.state == "ready")
                })
                && e.matches(&f)
        })
        .map(|e| e.kind.as_str())
        .collect();
    assert_eq!(selected, ["task.ready", "task.ready"]);
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
fn claim_expires_only_the_selected_overdue_lease() -> Result<()> {
    let d = tempfile::tempdir()?;
    let path = d.path().join("claims.db");
    let b = Board::open(&path)?;
    let opts = Default::default();
    let foreign = create_task(&b, "bob", new("ops", "unrelated", &opts), &[], None)?;
    let own = create_task(&b, "ada", new("ops", "mine", &opts), &[], None)?;
    for t in [&foreign, &own] {
        lease(&b, t.id)?;
    }
    let raw = rusqlite::Connection::open(&path)?;
    raw.execute("UPDATE tasks SET lease_until=0", [])?;
    let root = claim(&b, "ada", own.id, 30)?;
    assert_eq!(root.id, own.id);
    assert_eq!(root.epoch, 2);
    assert_eq!(
        raw.query_row("SELECT state FROM tasks WHERE id=?", [foreign.id], |r| {
            r.get::<_, String>(0)
        })?,
        "claimed"
    );
    assert!(claim(&b, "ada", own.id, 30).is_err());
    assert_eq!(
        raw.query_row("SELECT state FROM tasks WHERE id=?", [foreign.id], |r| {
            r.get::<_, String>(0)
        })?,
        "claimed"
    );
    Ok(())
}
