use anyhow::Result;
use board_local::{Board, PostOptions, access::Rules, automation::*, errors::classify};
use rusqlite::Connection;

const PROOF: &str = "precommitted-private-token";

#[test]
fn plans_commit_all_items_or_none_and_retry_without_duplicates() -> Result<()> {
    let b = Board::memory()?;
    let item: PlanItem = serde_json::from_value(serde_json::json!({"text":"work"}))?;
    let before = b.cursor()?;
    let publish = |items: &[PlanItem]| create_plan(&b, "ada", "work", items, Some("batch"));
    assert!(publish(&vec![item.clone(); 101]).is_err());
    let mut items = vec![item; 100];
    items[99].needs.push(Prerequisite::Id(999999));
    assert!(publish(&items).is_err());
    assert_eq!(b.cursor()?, before);
    items[99].needs.clear();
    let tasks = publish(&items)?;
    assert_eq!(tasks.len(), 100);
    let head = b.cursor()?;
    assert_eq!(publish(&items)?, tasks);
    assert_eq!(b.cursor()?, head);
    Ok(())
}

#[test]
fn atomic_plans_keep_input_order_and_distinguish_numeric_keys_from_task_ids() -> Result<()> {
    let board = Board::memory()?;
    let external = create_task(&board, "ada", new("inputs", &Default::default()), &[], None)?;
    let key = external.id.to_string();
    let items: Vec<PlanItem> = serde_json::from_value(serde_json::json!([
        {"text":"dependent","needs":[key, external.id]},
        {"text":"local prerequisite","key":key}
    ]))?;
    let tasks = create_plan(&board, "ada", "work", &items, None)?;
    assert!(tasks[0].id < tasks[1].id);
    assert_eq!(tasks[0].needs, vec![tasks[1].id, external.id]);
    let missing_key = create_plan(&board, "ada", "work", &items[..1], None).unwrap_err();
    assert_eq!(classify(&missing_key).1, "invalid_needs");
    for object in [
        serde_json::json!({"key":"x"}),
        serde_json::json!({"id":external.id}),
    ] {
        assert!(serde_json::from_value::<Prerequisite>(object).is_err());
    }
    Ok(())
}

#[test]
fn plan_adopts_the_conversation_in_place_without_claiming_or_rewriting_it() -> Result<()> {
    let board = Board::memory()?;
    let root = board.post(
        "incidents",
        "bo",
        "Investigate slow builds",
        &Default::default(),
    )?;
    let reply = board.reply(root.id, "bo", "Found the slow step", &Default::default())?;
    let items = serde_json::from_value::<Vec<PlanItem>>(serde_json::json!([
        {"text":"Verify the fix","needs":[root.id]},
        {"key":"fix","thread_id":root.id,"for":"builder"}
    ]))?;
    let batch = create_plan(&board, "ada", "build", &items, Some("fix-build"))?;
    assert_eq!(batch[0].needs, [root.id]);
    assert_eq!(batch[1].id, root.id);
    assert_eq!(batch[1].channel, "incidents");
    assert_eq!(batch[1].state, "ready");
    assert_eq!(batch[1].holder, None);
    assert_eq!(batch[1].assignee.as_deref(), Some("ada/builder"));
    let thread = board.conversation(root.id, &Default::default())?;
    assert_eq!(thread.message.text, root.text);
    assert_eq!(thread.replies[0].id, reply.id);
    let high = board.cursor()?;
    assert_eq!(
        create_plan(&board, "ada", "build", &items, Some("fix-build"))?,
        batch
    );
    assert_eq!(
        classify(&create_plan(&board, "ada", "build", &items, None).unwrap_err()).1,
        "already_a_task"
    );
    assert_eq!(board.cursor()?, high);
    Ok(())
}

#[test]
fn plan_rejects_invalid_adoptions_without_leaving_a_partial_batch() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("board");
    let b = Board::open(&path)?;
    let root = b.post("work", "ada", "live", &Default::default())?;
    let reply = b.reply(root.id, "ada", "not a root", &Default::default())?;
    let deleted = b.post("work", "ada", "deleted", &Default::default())?;
    b.delete(deleted.id, "ada", None)?;
    let secret = b.post("private", "ada", "hidden", &Default::default())?;
    b.set_grant("ada", None, "sam", &Rules::parse("work=read,post")?)?;
    let token = b.issue_credential("ada", "sam", None, None)?;
    let mut sam = Board::open(&path)?;
    sam.bind_token(&token.token)?;
    let high = b.cursor()?;
    let attempt = |db, actor, item| -> Result<_> {
        let items: Vec<PlanItem> =
            serde_json::from_value(serde_json::json!([{"text":"new task"},item]))?;
        create_plan(db, actor, "work", &items, None)
    };
    for invalid in [
        serde_json::json!({"thread_id":reply.id}),
        serde_json::json!({"thread_id":deleted.id}),
        serde_json::json!({"thread_id":root.id,"text":"replacement"}),
        serde_json::json!({"thread_id":root.id,"labels":{"x":"y"}}),
        serde_json::json!({"thread_id":root.id,"needs":[root.id]}),
    ] {
        assert!(attempt(&b, "ada", invalid).is_err());
    }
    assert_eq!(
        classify(&attempt(&sam, "sam", serde_json::json!({"thread_id":root.id})).unwrap_err()).1,
        "missing_permission"
    );
    assert_eq!(
        classify(&attempt(&sam, "sam", serde_json::json!({"thread_id":secret.id})).unwrap_err()).1,
        "message_not_found"
    );
    assert_eq!(b.cursor()?, high);
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
        claim_in_assignment(&b, "ann", dep.id, lease("private-ready-token"), None).map(|(t, _)| t)
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
fn scoped_tasks_hide_prerequisites_and_allow_claims_on_visible_work() -> Result<()> {
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
    assert_eq!(claim(&agent, "ann/w", z.id, 30)?.id, z.id);
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
fn loss_of_board_admission_fences_an_offline_watcher_even_with_everyone_grants() -> Result<()> {
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
    owner.revoke_grant("owner", admission.id)?;
    assert_eq!(classify(&client.cursor().unwrap_err()).1, "no_access");
    owner.post(
        "work/inbox",
        "owner",
        "outside membership",
        &Default::default(),
    )?;
    owner.set_grant("owner", None, "triage", &rules)?;
    let admitted = owner.post("work/inbox", "owner", "admitted again", &Default::default())?;
    assert_eq!(
        lease_event(&client, "events", 30)?.unwrap().event.cursor,
        admitted.id
    );
    Ok(())
}
