use anyhow::Result;
use board_local::{Board, Filter, PostOptions, automation::*};

fn make(
    db: &Board,
    actor: &str,
    text: &str,
    needs: &[i64],
    assignee: Option<&str>,
    key: Option<&str>,
) -> Result<Task> {
    let options = PostOptions {
        key: key.map(str::to_owned),
        ..Default::default()
    };
    create_task(
        db,
        actor,
        TaskTarget::New {
            channel: "work/items",
            text,
            options: &options,
        },
        needs,
        assignee,
    )
}

#[test]
fn claim_secrets_only_reach_the_authorized_lease_holder_and_fence_same_profile() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("held.db");
    let b = Board::open(&path)?;
    let t = make(&b, "ann", "triage", &[], Some("ann/agent"), None)?;
    let old = claim(&b, "ann/agent", t.id, 30)?;
    let token = old.claim_token.as_deref().unwrap();
    let renewed = change_task(
        &b,
        t.id,
        "ann/agent",
        Some(old.epoch),
        Some(token),
        TaskAction::Renew(90),
    )?;
    assert_eq!(renewed.claim_token.as_deref(), Some(token));
    assert_eq!(renewed.epoch, old.epoch);
    assert!(get_task(&b, t.id)?.claim_token.is_none());
    assert!(b.get(t.id)?.task.unwrap().claim_token.is_none());
    let log = serde_json::to_string(&b.events(0, 200)?)?;
    assert!(!log.contains(token) && !log.contains("claim_token"));
    for (actor, epoch, token) in [
        ("ann/other", Some(old.epoch), Some(token)),
        ("ann/agent", Some(old.epoch), None),
        ("ann/agent", Some(old.epoch), Some("wrong")),
    ] {
        assert!(change_task(&b, t.id, actor, epoch, token, TaskAction::Renew(30)).is_err());
        assert!(change_task(&b, t.id, actor, epoch, token, TaskAction::Complete(None)).is_err());
    }
    // Advance the persisted deadline rather than sleeping for a full lease in CI.
    rusqlite::Connection::open(&path)?
        .execute("UPDATE tasks SET lease_until=0 WHERE id=?", [t.id])?;
    let restarted = Board::open(path)?;
    let new = claim(&restarted, "ann/agent", t.id, 30)?;
    assert!(new.epoch > old.epoch);
    assert_ne!(old.claim_token, new.claim_token);
    let head = b.cursor()?;
    for epoch in [old.epoch, new.epoch] {
        assert!(
            change_task(
                &b,
                t.id,
                "ann/agent",
                Some(epoch),
                Some(token),
                TaskAction::Complete(Some("stale"))
            )
            .is_err()
        );
    }
    assert_eq!(b.cursor()?, head);
    let done = change_task(
        &restarted,
        t.id,
        "ann/agent",
        Some(new.epoch),
        new.claim_token.as_deref(),
        TaskAction::Complete(Some("done")),
    )?;
    assert_eq!(done.result.as_deref(), Some("done"));
    assert_eq!(b.get(t.id)?.task, Some(done));
    Ok(())
}

#[test]
fn single_task_is_a_keyed_post_but_a_batch_is_a_separate_guarded_request() -> Result<()> {
    let b = Board::memory()?;
    let t = make(&b, "ann", "work", &[], None, Some("task"))?;
    assert_eq!(make(&b, "ann", "work", &[], None, Some("task"))?.id, t.id);
    let head = b.cursor()?;
    assert_eq!(make(&b, "ann", "changed", &[], None, Some("task"))?, t);
    assert!(make(&b, "ann/other", "changed", &[], None, Some("task")).is_err());
    assert_eq!(
        b.post(
            "work/items",
            "ann",
            "work",
            &PostOptions {
                key: Some("task".into()),
                ..Default::default()
            }
        )?
        .task,
        Some(t)
    );
    assert_eq!(b.cursor()?, head);
    let opts = PostOptions {
        key: Some("msg".into()),
        ..Default::default()
    };
    let msg = b.post("work/items", "ann", "work", &opts)?;
    let head = b.cursor()?;
    assert!(make(&b, "ann", "work", &[], None, Some("msg")).is_err());
    assert!(b.get(msg.id)?.task.is_none());
    assert_eq!(b.cursor()?, head);
    let adopted = create_task(&b, "ann", TaskTarget::Existing(msg.id), &[], None)?;
    assert_eq!(
        make(&b, "ann", "reposted", &[], None, Some("msg"))?,
        adopted
    );
    let plan: Vec<PlanItem> = serde_json::from_str(r#"[{"text":"plan"}]"#)?;
    assert!(
        lookup_plan(&b, "ann", "msg").is_err(),
        "a single task is not a batch"
    );
    let (batch, made) = create_plan_with_outcome(&b, "ann", "work/items", &plan, Some("msg"))?;
    assert!(made);
    assert_ne!(batch[0].id, msg.id);
    assert_eq!(b.posted("ann", "msg")?.id, msg.id);
    let head = b.cursor()?;
    assert_eq!(
        create_plan_with_outcome(&b, "ann", "work/items", &plan, Some("msg"))?,
        (batch.clone(), false)
    );
    assert_eq!(lookup_plan(&b, "ann", "msg")?, batch);
    assert!(lookup_plan(&b, "bob", "msg").is_err());
    assert!(create_plan(&b, "bob", "work/items", &plan, Some("msg")).is_err());
    assert!(create_plan(&b, "ann", "different", &plan, Some("msg")).is_err());
    assert_eq!(b.cursor()?, head);
    Ok(())
}

#[test]
fn task_owner_assignment_reopen_dependencies_and_reserved_labels() -> Result<()> {
    let b = Board::memory()?;
    let a = make(
        &b,
        "ann/coordinator",
        "compile",
        &[],
        Some("ann/worker"),
        None,
    )?;
    let c = make(&b, "ann/coordinator", "test", &[a.id], None, None)?;
    assert!(
        change_task(
            &b,
            a.id,
            "ann/sibling",
            None,
            None,
            TaskAction::Cancel("no")
        )
        .is_err()
    );
    assert!(change_task(&b, a.id, "ann", None, None, TaskAction::Needs(&[c.id])).is_err());
    assert!(claim(&b, "ann", a.id, 60).is_err());
    let claimed = claim(&b, "ann/worker", a.id, 60)?;
    assert_eq!(a.id, claimed.id);
    change_task(
        &b,
        a.id,
        "ann/worker",
        Some(claimed.epoch),
        claimed.claim_token.as_deref(),
        TaskAction::Fail("retry"),
    )?;
    assert_eq!(get_task(&b, c.id)?.state, "waiting");
    change_task(&b, a.id, "ann", None, None, TaskAction::Resume)?;
    let held = claim(&b, "ann/worker", a.id, 60)?;
    let head = b.cursor()?;
    change_task(
        &b,
        a.id,
        "ann/worker",
        Some(held.epoch),
        held.claim_token.as_deref(),
        TaskAction::Complete(None),
    )?;
    let events = b.events(head, 5)?;
    assert_eq!(
        events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        ["task.completed", "task.ready"]
    );
    assert!(events[1].matches(&Filter {
        tasks_only: true,
        ..Default::default()
    }));
    let already_done = make(&b, "ann", "release", &[a.id], None, None)?;
    let events = b.events(already_done.id, 10)?;
    assert_eq!(
        events[0]
            .message
            .as_ref()
            .unwrap()
            .task
            .as_ref()
            .unwrap()
            .state,
        "waiting"
    );
    assert_eq!(events[1].kind, "task.ready");
    assert_eq!(already_done.state, "ready");
    let blocked = change_task(
        &b,
        c.id,
        "ann",
        None,
        None,
        TaskAction::Block("manual check"),
    )?;
    assert_eq!(blocked.state, "blocked");
    assert_eq!(
        change_task(&b, c.id, "ann", None, None, TaskAction::Resume)?.state,
        "ready"
    );
    assert_eq!(
        change_task(&b, c.id, "ann", None, None, TaskAction::Cancel("stop"))?.state,
        "canceled"
    );
    assert!(make(&b, "ann", "invalid", &[c.id], None, None).is_err());
    assert!(change_task(&b, c.id, "ann", None, None, TaskAction::Reopen).is_err());
    Ok(())
}

#[test]
fn task_thread_edits_and_deletion_invalidate_claims_atomically() -> Result<()> {
    let db = Board::memory()?;
    let m = db.post("project", "bo", "old request", &Default::default())?;
    let task = create_task(&db, "ann", TaskTarget::Existing(m.id), &[], None)?;
    let claim_task = || claim(&db, "bo/worker", task.id, 60);
    let claim = claim_task()?;
    let edit = db.edit(m.id, "bo", "changed request", Some(m.revision))?;
    assert_eq!(edit.task.unwrap().state, "blocked");
    assert!(
        change_task(
            &db,
            m.id,
            "bo/worker",
            Some(claim.epoch),
            claim.claim_token.as_deref(),
            TaskAction::Complete(None)
        )
        .is_err()
    );
    change_task(&db, m.id, "ann", None, None, TaskAction::Unblock)?;
    let claim = claim_task()?;
    let head = db.cursor()?;
    let removed = db.delete(m.id, "bo", Some(edit.revision))?;
    assert!(removed.deleted);
    assert_eq!(removed.task.unwrap().state, "canceled");
    assert_eq!(
        db.events(head, 5)?
            .iter()
            .map(|e| e.kind.as_str())
            .collect::<Vec<_>>(),
        ["task.canceled", "message.deleted"]
    );
    assert!(
        change_task(
            &db,
            m.id,
            "bo/worker",
            Some(claim.epoch),
            claim.claim_token.as_deref(),
            TaskAction::Complete(None)
        )
        .is_err()
    );
    Ok(())
}
