use anyhow::Result;
use board_local::{Board, PostOptions, access::Rules, automation::*, errors::classify};
use rusqlite::Connection;
use std::collections::BTreeMap;

const PROOF: &str = "precommitted-private-token";

#[test]
fn large_plans_checkpoint_chunks_and_allow_repairs_only_to_uncommitted_items() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("plans.db");
    let board = Board::open(&db)?;
    let mut raw: Vec<_> = (0..105)
        .map(|i| serde_json::json!({"key":format!("k{i}"),"text":format!("step {i}")}))
        .collect();
    raw[0]["needs"] = serde_json::json!([{"key":"k104"}]);
    let items: Vec<PlanItem> = serde_json::from_value(serde_json::json!(raw))?;
    let inspect = Connection::open(&db)?;
    inspect.execute_batch("CREATE TRIGGER stop_later BEFORE INSERT ON messages WHEN NEW.text='step 101' BEGIN SELECT RAISE(ABORT,'storage interrupted'); END;")?;
    assert!(create_plan(&board, "ada", "work", &items, Some("durable-plan")).is_err());
    let committed = lookup_plan(&board, "ada", "durable-plan")?;
    assert_eq!(committed.len(), 100);
    assert_eq!(committed[0].text, "step 1");
    assert!(lookup_plan(&board, "outsider", "durable-plan").is_err());
    drop(board);
    let other = Board::open(&db)?;
    let taken = claim(
        &other,
        "ada/worker",
        ClaimTarget::Ready("work"),
        30,
        &Default::default(),
    )?;
    assert_eq!(taken.id, committed[0].id);
    inspect.execute_batch("DROP TRIGGER stop_later")?;
    let mut changed = items.clone();
    changed[5].text = "rewrote committed".into();
    let high = other.cursor()?;
    assert_eq!(
        classify(&create_plan(&other, "ada", "work", &changed, Some("durable-plan")).unwrap_err())
            .1,
        "duplicate_id"
    );
    assert_eq!(other.cursor()?, high);
    let mut repaired = items;
    repaired[101].text = "repaired tail".into();
    let out = create_plan(&other, "ada", "work", &repaired, Some("durable-plan"))?;
    assert_eq!(out.len(), 105);
    assert_eq!(out[1].id, taken.id);
    assert_eq!(out[1].state, "claimed");
    assert_eq!(out[0].needs, vec![out[104].id]);
    assert_eq!(out[101].text, "repaired tail");
    let high = other.cursor()?;
    assert_eq!(
        create_plan(&other, "ada", "work", &repaired, Some("durable-plan"))?,
        out
    );
    assert_eq!(other.cursor()?, high);
    repaired.push(serde_json::from_value(serde_json::json!({"text":"extra"}))?);
    assert_eq!(
        classify(&create_plan(&other, "ada", "work", &repaired, Some("durable-plan")).unwrap_err())
            .1,
        "duplicate_id"
    );
    Ok(())
}

#[test]
fn blind_schedules_only_reveal_ones_created_blind_and_retain_temporary_caps() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("blind.db");
    let mut b = Board::open(&db)?;
    let policy = Rules::parse("private=read,post")?;
    b.set_grant("owner", None, "ada/bot", &policy)?;
    let credential = b.issue_credential("owner", "ada/bot", None, None)?;
    let other = b.post("private", "other", "not public", &Default::default())?;
    let visible = b.with_request(Some(&credential.token), "owner", None, |b, actor| {
        schedule(
            b,
            actor,
            Destination::Channel("private"),
            "originally readable",
            due(),
            Default::default(),
        )
    })?;
    let blind = b.with_request(
        Some(&credential.token),
        "owner",
        Some("post"),
        |b, actor| {
            assert!(
                schedule(
                    b,
                    actor,
                    Destination::Reply(other.id),
                    "no thread access",
                    due(),
                    Default::default()
                )
                .is_err()
            );
            let blind = schedule(
                b,
                actor,
                Destination::Channel("private"),
                "blind",
                due(),
                Default::default(),
            )?;
            let c = schedule(
                b,
                actor,
                Destination::Channel("private"),
                "will cancel",
                due(),
                Default::default(),
            )?;
            cancel_schedule(b, c.id, actor)?;
            assert_eq!(
                schedules(b, false)?
                    .iter()
                    .map(|s| s.id)
                    .collect::<Vec<_>>(),
                vec![blind.id]
            );
            assert!(get_schedule(b, visible.id).is_err());
            assert!(cancel_schedule(b, visible.id, actor).is_err());
            Ok(blind)
        },
    )?;
    drop(b);
    let mut b = Board::open(&db)?;
    assert_eq!(dispatch_due(&b)?, 2);
    let root = b
        .messages(&board_local::Filter::default(), 100)?
        .into_iter()
        .find(|m| m.schedule.as_ref().is_some_and(|s| s.id == blind.id))
        .unwrap();
    b.with_request(
        Some(&credential.token),
        "owner",
        Some("post"),
        |b, actor| {
            assert!(b.get(other.id).is_err());
            // The original cap, not the worker's local-owner authority, marked the send as blind.
            b.edit(root.id, actor, "my correction", None)?;
            Ok(())
        },
    )?;
    Ok(())
}
fn lease(token: &str) -> ClaimRequest<'_> {
    ClaimRequest {
        lease_seconds: 30,
        token,
    }
}
fn new<'a>(channel: &'a str, options: &'a PostOptions) -> TaskTarget<'a> {
    TaskTarget::New {
        channel,
        text: "work",
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

#[test]
fn creation_adoption_and_claim_are_atomic_and_lost_responses_recover_after_restart() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("board.db");
    let b = Board::open(&db)?;
    let options = PostOptions {
        key: Some("job".into()),
        ..Default::default()
    };
    let dep = create_task(&b, "ann", new("ops", &Default::default()), &[], None)?;
    let head = b.cursor()?;
    let rejected = create_task_and_claim(
        &b,
        "ann",
        new("ops", &options),
        &[dep.id],
        None,
        Some(lease(PROOF)),
    );
    assert!(rejected.is_err());
    assert_eq!(b.cursor()?, head);
    let root = b.post("ops", "ann", "adopt", &Default::default())?;
    assert!(
        create_task_and_claim(
            &b,
            "ann",
            TaskTarget::Existing(root.id),
            &[dep.id],
            None,
            Some(lease(PROOF))
        )
        .is_err()
    );
    assert!(b.get(root.id)?.task.is_none());
    let (adopted, first) = create_task_and_claim(
        &b,
        "ann",
        TaskTarget::Existing(root.id),
        &[],
        None,
        Some(lease("minted-adopt-token")),
    )?;
    assert!(first);
    assert_eq!(adopted.state, "claimed");
    assert!(
        !create_task_and_claim(
            &b,
            "ann",
            TaskTarget::Existing(root.id),
            &[],
            None,
            Some(lease("minted-adopt-token"))
        )?
        .1
    );
    let (created, made) = create_task_and_claim(
        &b,
        "ann",
        new("ops", &options),
        &[],
        None,
        Some(lease(PROOF)),
    )?;
    assert!(made);
    let high = b.cursor()?;
    drop(b);
    let b = Board::open(&db)?;
    let (again, made) = create_task_and_claim(
        &b,
        "ann",
        new("ops", &options),
        &[],
        None,
        Some(lease(PROOF)),
    )?;
    assert!(!made);
    assert_eq!(again, created);
    assert_eq!(recover_claim(&b, "ann", PROOF)?, created);
    assert!(recover_claim(&b, "sam", PROOF).is_err());
    assert_eq!(b.cursor()?, high);
    assert!(!serde_json::to_string(&b.events(0, 100)?)?.contains(PROOF));
    let q = || {
        claim_with_token(
            &b,
            "ann",
            ClaimTarget::Ready("ops"),
            lease("private-ready-token"),
            &BTreeMap::new(),
            &[],
        )
    };
    let one = q()?;
    let prior = b.cursor()?;
    assert_eq!(q()?, one);
    assert_eq!(b.cursor()?, prior);
    change_task(
        &b,
        one.id,
        "ann",
        Some(one.epoch),
        one.claim_token.as_deref(),
        TaskAction::Release,
    )?;
    assert_eq!(classify(&q().unwrap_err()).1, "claim_lost");
    let old = Connection::open(&db)?;
    old.execute("UPDATE tasks SET lease_until=0 WHERE id=?", [created.id])?;
    assert_eq!(
        classify(&recover_claim(&b, "ann", PROOF).unwrap_err()).1,
        "claim_lost"
    );
    Ok(())
}

#[test]
fn scheduled_files_survive_restart_and_permission_loss_cancels_only_revoked_sends() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("board.db");
    let owner = Board::open(&db)?;
    let root = owner.post("ops", "ann", "root", &Default::default())?;
    let child = owner.reply(root.id, "ann", "exact reply", &Default::default())?;
    owner.set_grant(
        "ann",
        None,
        "ann/worker",
        &Rules::parse("ops=read,post,label")?,
    )?;
    let issued = owner.issue_credential("ann", "ann/worker", None, None)?;
    let mut agent = Board::open(&db)?;
    agent.bind_token(&issued.token)?;
    let reusable = agent.upload("ann/worker", "ops", "reused.txt", "text/plain", b"reusable")?;
    let opts = PostOptions {
        files: vec![("bytes.bin".into(), vec![0, 255, 4])],
        attachment_ids: vec![reusable.id],
        ..Default::default()
    };
    let s = schedule_post(
        &agent,
        "ann/worker",
        Destination::Reply(child.id),
        "",
        due(),
        &opts,
    )?;
    assert_eq!(s.attachments[0].id, Some(reusable.id));
    assert_eq!(s.attachments[1].size, 3);
    assert_eq!(
        schedule_query(&agent, false, Some(root.id), Some("ann"), 0, 100, false)?.1[0].id,
        s.id
    );
    assert!(dispatch_due(&agent).is_err());
    drop(agent);
    drop(owner);
    let owner = Board::open(&db)?;
    assert_eq!(dispatch_due(&owner)?, 1);
    let reply = owner.thread(root.id)?.pop().unwrap();
    assert_eq!(reply.reply_to, Some(child.id));
    assert_eq!(reply.attachments[0].id, reusable.id);
    assert_eq!(owner.download(reply.attachments[1].id)?.1, vec![0, 255, 4]);
    let mut agent = Board::open(&db)?;
    agent.bind_token(&issued.token)?;
    let revoked = schedule_post(
        &agent,
        "ann/worker",
        Destination::Channel("ops"),
        "",
        due(),
        &opts,
    )?;
    owner.revoke_credential("ann", &issued.credential.id)?;
    schedule(
        &owner,
        "ann",
        Destination::Channel("other"),
        "ok",
        due(),
        Default::default(),
    )?;
    assert_eq!(dispatch_due(&owner)?, 1);
    assert_eq!(get_schedule(&owner, revoked.id)?.state, "revoked");
    Ok(())
}

#[test]
fn scoped_tasks_hide_prerequisites_and_queue_limits_do_not_hide_allowed_work() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("access.db");
    let b = Board::open(&db)?;
    let x = create_task(
        &b,
        "ann",
        new("work/private", &Default::default()),
        &[],
        None,
    )?;
    let y = create_task(
        &b,
        "ann",
        new("work/shared", &Default::default()),
        &[x.id],
        None,
    )?;
    let z = create_task(
        &b,
        "ann",
        new("work/shared", &Default::default()),
        &[],
        None,
    )?;
    b.set_grant(
        "a",
        None,
        "ann/w",
        &Rules::parse("work/shared=read,post,label")?,
    )?;
    let issued = b.issue_credential("ann", "ann/w", None, None)?;
    let mut agent = Board::open(&db)?;
    agent.bind_token(&issued.token)?;
    assert!(get_task(&agent, x.id).is_err());
    assert!(get_task(&agent, y.id)?.needs.is_empty());
    assert_eq!(tasks_page(&agent, None, None, 0, 1)?.1[0].id, y.id);
    assert!(
        create_task(
            &agent,
            "ann/w",
            new("work/shared", &Default::default()),
            &[x.id],
            None
        )
        .is_err()
    );
    assert!(
        filtered_tasks_page(
            &agent,
            &TaskFilter {
                needs: Some(x.id),
                ..Default::default()
            },
            0,
            10
        )
        .is_err()
    );
    assert!(
        list_tasks(&agent, None, None)?
            .iter()
            .all(|t| t.channel == "work/shared")
    );
    assert_eq!(
        claim(
            &agent,
            "ann/w",
            ClaimTarget::Ready("work/"),
            30,
            &Default::default()
        )?
        .id,
        z.id
    );
    assert!(watchers(&agent)?.is_empty());
    assert!(expire_leases(&agent).is_err());
    Ok(())
}

#[test]
fn task_hold_kind_is_preserved_in_events_and_the_edit_kind_is_reserved() -> Result<()> {
    let db = Board::memory()?;
    let root = db.post("ops", "bo", "work", &Default::default())?;
    create_task(&db, "ada", TaskTarget::Existing(root.id), &[], None)?;
    let start = db.cursor()?;
    assert!(
        change_task(
            &db,
            root.id,
            "ada",
            None,
            None,
            TaskAction::BlockWithKind("reserved", "edited")
        )
        .is_err()
    );
    assert_eq!(db.cursor()?, start);
    let held = change_task(
        &db,
        root.id,
        "ada",
        None,
        None,
        TaskAction::BlockWithKind("review first", "waiting-review"),
    )?;
    assert_eq!(held.hold_kind.as_deref(), Some("waiting-review"));
    assert_eq!(
        db.events(start, 10)?[0].payload["task"]["hold_kind"],
        "waiting-review"
    );
    change_task(&db, root.id, "ada", None, None, TaskAction::Unblock)?;
    assert!(get_task(&db, root.id)?.hold_kind.is_none());
    db.edit(root.id, "bo", "changed", None)?;
    assert_eq!(get_task(&db, root.id)?.hold_kind.as_deref(), Some("edited"));
    Ok(())
}

#[test]
fn delegated_watchers_use_private_names_and_recheck_both_registered_and_pulling_permissions()
-> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("watch-auth.db");
    let owner = Board::open(&db)?;
    let full = Rules::parse("work/=read,post")?;
    let grant = owner.set_grant("owner", None, "ada/triage", &full)?;
    owner.set_grant("owner", None, "bo/triage", &full)?;
    let make = |actor: &str, cap: Option<&Rules>| -> Result<Board> {
        let issued = owner.issue_credential("owner", actor, cap, None)?;
        let mut b = Board::open(&db)?;
        b.bind_token(&issued.token)?;
        Ok(b)
    };
    owner.post("work/a", "human", "seed", &Default::default())?;
    owner.post("work/b", "human", "seed", &Default::default())?;
    let ada = make("ada/triage", None)?;
    let bo = make("bo/triage", None)?;
    let aonly = make("ada/triage", Some(&Rules::parse("work/a=read")?))?;
    let add = |b: &Board, actor: &str| {
        add_watcher_as(
            b,
            actor,
            "jobs",
            board_local::Filter {
                channel: Some("work/".into()),
                ..Default::default()
            },
            Some("message.posted"),
        )
    };
    add(&ada, "ada/triage")?;
    assert!(get_watcher(&bo, "jobs").is_err());
    assert!(remove_watcher_as(&bo, "bo/triage", "jobs").is_err());
    add(&bo, "bo/triage")?;
    assert_eq!(watchers(&ada)?.len(), 1);
    assert_eq!(watchers(&bo)?[0].run_as.as_deref(), Some("bo/triage"));
    let hidden = owner.post("other", "human", "never visible", &Default::default())?;
    let b = owner.post(
        "work/b",
        "human",
        "temporarily readable",
        &Default::default(),
    )?;
    let a = owner.post("work/a", "human", "stays readable", &Default::default())?;
    let d = lease_event(&ada, "jobs", 120)?.unwrap();
    assert_eq!(d.event.cursor, b.id);
    assert!(ack_delivery(&aonly, "jobs", b.id, Some(&d.receipt)).is_err());
    assert_eq!(next_event(&aonly, "jobs")?.unwrap().cursor, a.id);
    ack_event(&aonly, "jobs", a.id)?;
    let completed = owner.post(
        "work/b",
        "human",
        "settled while visible",
        &Default::default(),
    )?;
    let finished = lease_event(&ada, "jobs", 60)?.unwrap();
    assert_eq!(finished.event.cursor, completed.id);
    let prove = |db: &Board, id| {
        settle_delivery_if(
            db,
            "jobs",
            id,
            Some(&finished.receipt),
            DeliveryOutcome::Ack,
            true,
        )
    };
    prove(&ada, completed.id)?;
    assert_eq!(
        classify(&prove(&ada, completed.id).unwrap_err()).1,
        "no_match"
    );
    let masked = prove(&aonly, completed.id).unwrap_err();
    assert_eq!(classify(&masked).1, "delivery_conflict");
    assert_eq!(
        masked.to_string(),
        prove(&aonly, hidden.id).unwrap_err().to_string()
    );
    assert_eq!(
        masked.to_string(),
        prove(&bo, completed.id).unwrap_err().to_string()
    );
    owner.set_grant(
        "owner",
        Some(grant.id),
        "ada/triage",
        &Rules::parse("work/a=read,post")?,
    )?;
    assert!(ack_delivery(&ada, "jobs", b.id, Some(&d.receipt)).is_err());
    assert_eq!(
        masked.to_string(),
        prove(&ada, completed.id).unwrap_err().to_string()
    );
    Connection::open(&db)?.execute(
        "UPDATE watcher_queue SET lease_until=0 WHERE cursor=?",
        [b.id],
    )?;
    assert!(lease_event(&ada, "jobs", 60)?.is_none());
    let bo_event = next_event(&bo, "jobs")?.unwrap();
    assert_eq!(bo_event.cursor, b.id);
    assert_ne!(bo_event.cursor, hidden.id);
    assert!(get_watcher(&ada, "jobs")?.cursor >= a.id);
    // A broader grant at first pull must not retrospectively create matches.
    owner.post(
        "work/b",
        "human",
        "not readable when posted",
        &Default::default(),
    )?;
    owner.set_grant("owner", Some(grant.id), "ada/triage", &full)?;
    let newly_readable = owner.post("work/b", "human", "readable now", &Default::default())?;
    assert_eq!(
        lease_event(&ada, "jobs", 60)?.unwrap().event.cursor,
        newly_readable.id
    );
    Ok(())
}

#[test]
fn loss_of_board_admission_fences_a_due_schedule_and_offline_watcher_even_with_everyone_grants()
-> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("admission.db");
    let owner = Board::open(&path)?;
    let rules = Rules::parse("work/=read,post")?;
    owner.set_grant("owner", None, "*", &rules)?;
    let admission = owner.set_grant("owner", None, "triage", &rules)?;
    let issued = owner.issue_credential("owner", "triage", None, None)?;
    let mut client = Board::open(&path)?;
    client.bind_token(&issued.token)?;
    add_watcher_as(
        &client,
        "triage",
        "events",
        board_local::Filter::default(),
        Some("message.posted"),
    )?;
    let later = schedule(
        &client,
        "triage",
        Destination::Channel("work/inbox"),
        "must not fire",
        Timing {
            due: chrono::Utc::now().timestamp(),
            every: None,
            times: None,
            until: None,
        },
        Default::default(),
    )?;
    owner.revoke_grant("owner", admission.id)?;
    assert_eq!(classify(&client.cursor().unwrap_err()).1, "no_access");
    owner.post(
        "work/inbox",
        "owner",
        "outside membership",
        &Default::default(),
    )?;
    assert_eq!(dispatch_due(&owner)?, 0);
    assert_eq!(get_schedule(&owner, later.id)?.state, "revoked");
    owner.set_grant("owner", None, "triage", &rules)?;
    let admitted = owner.post("work/inbox", "owner", "admitted again", &Default::default())?;
    assert_eq!(
        lease_event(&client, "events", 30)?.unwrap().event.cursor,
        admitted.id
    );
    Ok(())
}
