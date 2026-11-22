use anyhow::Result;
use board_local::{Board, PostOptions, append_block, swarm::*};
use serde_json::{Value, json};

fn board() -> Result<Board> {
    let b = Board::memory()?;
    b.swarm_start(
        "ada",
        &SwarmStart {
            goal: "Verify a revision".into(),
            slug: Some("audit".into()),
            ..Default::default()
        },
    )?;
    for name in ["impl", "verify"] {
        b.swarm_add_worker(
            "ada",
            "audit",
            &SwarmAddWorker {
                name: name.into(),
                brief: "Own your slice".into(),
                ..Default::default()
            },
        )?;
    }
    Ok(b)
}

#[test]
fn registration_is_not_execution_and_activity_does_not_imply_launch_failure() -> Result<()> {
    let b = board()?;
    let worker = &b.swarm_status("ada", "audit")?.workers[0];
    assert_eq!(worker.status, "registered");
    assert!(worker.last_activity_at.is_none());
    assert!(b.swarm_check_in("ada/verify", "audit", "impl").is_err());
    b.swarm_check_in("ada/impl", "audit", "impl")?;
    b.swarm_post(
        "ada/impl",
        "audit",
        "Editing the implementation",
        &Default::default(),
    )?;
    let run = b.swarm_status("ada", "audit")?;
    assert_eq!(run.workers[0].status, "working");
    assert!(run.workers[0].last_activity_at.is_some());
    assert_eq!(run.workers[1].status, "registered");
    assert!(
        b.swarm_launch_failed("ada/impl", "audit", "verify", "Harness rejected launch")
            .is_err()
    );
    b.swarm_launch_failed("ada", "audit", "verify", "Harness rejected launch")?;
    let run = b.swarm_status("ada", "audit")?;
    assert_eq!(run.workers[1].status, "launch_failed");
    assert_eq!(
        run.workers[1].launch_error.as_deref(),
        Some("Harness rejected launch")
    );
    b.swarm_add_worker(
        "ada",
        "audit",
        &SwarmAddWorker {
            name: "review".into(),
            brief: "Review".into(),
            ..Default::default()
        },
    )?;
    b.swarm_launch_failed("ada", "audit", "review", "Harness rejected launch")?;
    let view = b.swarm_observe("ada", "audit", None, 0, 10)?;
    assert_eq!(
        view.actions
            .iter()
            .filter_map(|a| a.worker.as_deref())
            .collect::<Vec<_>>(),
        ["verify", "review"]
    );
    Ok(())
}

#[test]
fn completed_input_does_not_block_dependents_while_its_handoff_awaits_review() -> Result<()> {
    use board_local::automation::{
        ClaimRequest, TaskAction, TaskTarget, change_task, create_task, create_task_and_claim,
    };
    let b = board()?;
    let opts = PostOptions::default();
    let target = |text| TaskTarget::New {
        channel: "swarms/audit",
        text,
        options: &opts,
    };
    let token = "a-private-test-token";
    let (a, _) = create_task_and_claim(
        &b,
        "ada/impl",
        target("Produce artifact"),
        &[],
        None,
        Some(ClaimRequest {
            token,
            lease_seconds: 60,
        }),
    )?;
    let other = create_task(&b, "ada/impl", target("Other input"), &[], None)?;
    create_task(
        &b,
        "ada/impl",
        target("Combine both"),
        &[a.id, other.id],
        None,
    )?;
    post(
        &b,
        "ada/impl",
        "handoff",
        json!({"revision":{"kind":"sha256","value":"a".repeat(64)},"task":a.id,"summary":"Ready","changes":["artifact"],"to":"verify"}),
    )?;
    assert_eq!(
        b.swarm_observe("ada", "audit", None, 0, 1)?.actions[0].blocked_dependents,
        1
    );
    change_task(
        &b,
        a.id,
        "ada/impl",
        Some(a.epoch),
        Some(token),
        TaskAction::Complete(None),
    )?;
    let obs = b.swarm_observe("ada", "audit", None, 0, 1)?;
    assert_eq!(obs.tasks["waiting"], 1);
    assert_eq!(obs.actions[0].blocked_dependents, 0);
    Ok(())
}

fn post(b: &Board, who: &str, schema: &str, data: Value) -> Result<i64> {
    Ok(b.swarm_post(
        who,
        "audit",
        &append_block("", schema, 1, &data)?,
        &PostOptions::default(),
    )?
    .message
    .id)
}

#[test]
fn receipt_binds_checks_to_the_exact_handoff_and_resolves_explicit_decisions() -> Result<()> {
    let b = board()?;
    let revision = json!({"kind":"sha256","value":"a".repeat(64)});
    let handoff = post(
        &b,
        "ada/impl",
        "handoff",
        json!({"revision":revision,"summary":"Fix Unicode splitting","changes":["fields.rs"],"to":"verify"}),
    )?;
    assert!(post(&b, "ada/verify", "verification", json!({"handoff":handoff,"revision":{"kind":"sha256","value":"b".repeat(64)},"summary":"Wrong revision","checks":[{"name":"Unicode","status":"pass","command":"cargo test unicode"}]})).is_err());
    post(
        &b,
        "ada/verify",
        "verification",
        json!({"handoff":handoff,"revision":revision,"summary":"Unicode passes; macOS not available","checks":[{"name":"Unicode","status":"pass","command":"cargo test unicode"},{"name":"macOS","status":"not-run","command":"cargo test","output":"No Mac available"}]}),
    )?;
    let decision = post(
        &b,
        "ada/verify",
        "decision-request",
        json!({"to":"ada","summary":"Is Linux-only verification acceptable?","handoff":handoff}),
    )?;
    let receipt = b.swarm_receipt("ada", "audit")?;
    assert_eq!(receipt.handoffs[0].status, "not-run");
    let snapshot = b.swarm_observe("ada", "audit", None, 0, 20)?;
    assert!(
        snapshot
            .actions
            .iter()
            .any(|a| a.kind == "verification" && a.assigned_to == "ada/impl")
    );
    assert!(
        snapshot
            .actions
            .iter()
            .any(|a| a.message_id == decision && a.assigned_to == "ada")
    );
    assert_eq!(receipt.decisions.len(), 1);
    assert_eq!(
        b.attention("ada")?
            .iter()
            .filter(|i| i.action_required)
            .count(),
        1
    );
    assert!(
        post(
            &b,
            "ada/impl",
            "decision-resolution",
            json!({"request":decision,"summary":"I approve"})
        )
        .is_err()
    );
    post(
        &b,
        "ada",
        "decision-resolution",
        json!({"request":decision,"summary":"Accepted for this local Linux build"}),
    )?;
    assert!(b.swarm_receipt("ada", "audit")?.decisions.is_empty());
    let newer = post(
        &b,
        "ada/impl",
        "handoff",
        json!({"revision":{"kind":"sha256","value":"b".repeat(64)},"summary":"Follow-up change","changes":["fields.rs"],"to":"verify"}),
    )?;
    let receipt = b.swarm_receipt("ada", "audit")?;
    let current = b.swarm_observe("ada", "audit", None, 0, 20)?;
    assert_eq!(current.actions.len(), 1);
    assert_eq!(
        (
            current.actions[0].message_id,
            current.actions[0].kind.as_str()
        ),
        (newer, "awaiting_verification")
    );
    assert!(
        receipt
            .handoffs
            .iter()
            .find(|h| h.id == handoff)
            .unwrap()
            .superseded
    );
    assert_eq!(
        receipt
            .handoffs
            .iter()
            .find(|h| h.id == newer)
            .unwrap()
            .status,
        "unverified"
    );
    b.swarm_stop("ada", "audit")?;
    assert_eq!(b.swarm_receipt("ada", "audit")?.run_status, "stopped");
    Ok(())
}

#[test]
fn handoffs_notify_verifiers_and_replayed_posts_do_not_duplicate_receipts() -> Result<()> {
    let b = board()?;
    let data = json!({"revision":{"kind":"git-commit","value":"a".repeat(40)},"summary":"Ready for review","changes":["src/lib.rs"],"to":"verify"});
    let body = append_block("", "handoff", 1, &data)?;
    let options = PostOptions {
        key: Some("handoff-a".into()),
        ..Default::default()
    };
    let first = b.swarm_post("ada/impl", "audit", &body, &options)?;
    assert_eq!(first.notified, ["verify"]);
    let replay = b.swarm_post("ada/impl", "audit", &body, &options)?;
    assert_eq!(replay.message.id, first.message.id);
    assert_eq!(b.swarm_receipt("ada", "audit")?.handoffs.len(), 1);
    assert!(post(&b,"ada/impl","verification",json!({"handoff":first.message.id,"revision":data["revision"],"summary":"Self certification","checks":[{"name":"unit","status":"pass","command":"cargo test"}]})).is_err());
    let fail = post(
        &b,
        "ada/verify",
        "verification",
        json!({"handoff":first.message.id,"revision":data["revision"],"summary":"Fix Unicode","checks":[{"name":"unit","status":"fail","command":"cargo test","output":"failed"}]}),
    )?;
    assert!(
        b.attention("ada")?
            .iter()
            .any(|i| i.message_id == fail && !i.action_required)
    );
    let request = post(
        &b,
        "ada/verify",
        "decision-request",
        json!({"to":"ada","summary":"Should we change the public signature?"}),
    )?;
    b.swarm_stop("ada", "audit")?;
    b.swarm_resolve_decision("ada", "audit", request, "Keep the signature")?;
    assert!(b.swarm_receipt("ada", "audit")?.decisions.is_empty());
    assert!(
        b.swarm_resolve_decision("ada", "audit", request, "Second answer")
            .is_err()
    );
    Ok(())
}

#[test]
fn upgrade_preserves_assignments_and_does_not_fabricate_old_working_checkins() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("legacy.sqlite");
    let b = Board::open(&path)?;
    b.swarm_start(
        "ada",
        &SwarmStart {
            goal: "Legacy run".into(),
            slug: Some("audit".into()),
            ..Default::default()
        },
    )?;
    b.swarm_add_worker(
        "ada",
        "audit",
        &SwarmAddWorker {
            name: "impl".into(),
            brief: "Original assignment".into(),
            ..Default::default()
        },
    )?;
    b.swarm_check_in("ada/impl", "audit", "impl")?;
    let original = b.swarm_status("ada", "audit")?.workers[0].clone();
    drop(b);
    let fixture = rusqlite::Connection::open(&path)?;
    fixture.execute_batch("ALTER TABLE swarm_workers DROP COLUMN checked_in_at; ALTER TABLE swarm_workers DROP COLUMN last_activity_at; ALTER TABLE swarm_workers DROP COLUMN launch_error; PRAGMA user_version=16;")?;
    drop(fixture);
    let b = Board::open(&path)?;
    let current = b.swarm_status("ada", "audit")?.workers[0].clone();
    assert_eq!(current.id, original.id);
    assert_eq!(current.brief, "Original assignment");
    assert_eq!(current.status, "registered");
    assert!(current.checked_in_at.is_none() && current.last_activity_at.is_none());
    b.swarm_check_in("ada/impl", "audit", "impl")?;
    assert_eq!(b.swarm_status("ada", "audit")?.workers[0].status, "working");
    Ok(())
}

#[test]
fn attention_and_receipts_respect_channel_access() -> Result<()> {
    use board_local::access::Rules;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("private.sqlite");
    let owner = Board::open(&path)?;
    owner.swarm_start(
        "ada",
        &SwarmStart {
            goal: "Private run".into(),
            slug: Some("private".into()),
            ..Default::default()
        },
    )?;
    owner.swarm_post(
        "ada",
        "private",
        &append_block(
            "",
            "decision-request",
            1,
            &json!({"to":"guest","summary":"Private question"}),
        )?,
        &Default::default(),
    )?;
    owner.set_grant("ada", None, "guest", &Rules::parse("public=read,post")?)?;
    let token = owner.issue_credential("ada", "guest", None, None)?.token;
    let mut guest = Board::open(&path)?;
    guest.bind_token(&token)?;
    assert!(guest.attention("guest")?.is_empty());
    assert!(guest.swarm_receipt("guest", "private").is_err());
    assert!(guest.swarm_observe("guest", "private", None, 0, 2).is_err());
    let grant = owner.set_grant("ada", None, "guest", &Rules::parse("swarms/private=read")?)?;
    assert_eq!(
        guest.swarm_observe("guest", "private", None, 0, 2)?.actions[0].assigned_to,
        "guest"
    );
    owner.revoke_grant("ada", grant.id)?;
    assert!(
        guest
            .swarm_observe("guest", "private", Some(0), 0, 2)
            .is_err()
    );
    Ok(())
}

#[test]
fn v17_policy_upgrade_preserves_role_customization_and_uses_defaults_for_existing_runs()
-> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("previous");
    let b = Board::open(&path)?;
    b.swarm_start(
        "ada",
        &SwarmStart {
            slug: Some("old".into()),
            goal: "Preserve history".into(),
            ..Default::default()
        },
    )?;
    b.role_detach_skill("implementer", "board-implementer")?;
    let head = b.cursor()?;
    drop(b);
    rusqlite::Connection::open(&path)?.execute_batch(
        "DROP TABLE swarm_plans; DROP TABLE swarm_policy; PRAGMA user_version=17;",
    )?;
    let b = Board::open(&path)?;
    assert!(
        !b.role_show("implementer")?
            .skills
            .iter()
            .any(|s| s == "board-implementer")
    );
    assert_eq!(
        b.swarm_observe("ada", "old", None, 0, 1)?
            .available_capacity,
        8
    );
    assert_eq!(b.cursor()?, head);
    Ok(())
}

#[test]
fn agent_addressed_work_is_followup_for_the_operator_not_a_human_action() -> Result<()> {
    use board_local::automation::{TaskAction, TaskTarget, change_task, create_task};
    let b = board()?;
    let task = create_task(
        &b,
        "ada",
        TaskTarget::New {
            channel: "swarms/audit",
            text: "Repair the failing check",
            options: &Default::default(),
        },
        &[],
        Some("ada/impl"),
    )?;
    change_task(
        &b,
        task.id,
        "ada",
        None,
        None,
        TaskAction::Block("Verifier returned a failing case"),
    )?;
    let request = post(
        &b,
        "ada/impl",
        "decision-request",
        json!({"to":"ada/verify","summary":"Which fixture reproduces the failure?"}),
    )?;
    let operator = b.attention("ada")?;
    assert_eq!(
        operator
            .iter()
            .find(|i| i.message_id == task.id)
            .unwrap()
            .waiting,
        "ada/impl"
    );
    for id in [task.id, request] {
        assert!(
            !operator
                .iter()
                .find(|i| i.message_id == id)
                .unwrap()
                .action_required
        );
    }
    assert!(
        b.attention("ada/impl")?
            .iter()
            .find(|i| i.message_id == task.id)
            .unwrap()
            .action_required
    );
    assert!(
        b.attention("ada/verify")?
            .iter()
            .find(|i| i.message_id == request)
            .unwrap()
            .action_required
    );
    assert!(
        b.swarm_receipt("ada", "audit")?
            .open_tasks
            .contains(&task.id)
    );
    Ok(())
}

#[test]
fn ordinary_swarm_channel_posts_and_replies_record_the_same_evidence() -> Result<()> {
    let b = board()?;
    let revision = json!({"kind":"sha256","value":"a".repeat(64)});
    let handoff_body = append_block(
        "",
        "handoff",
        1,
        &json!({"revision":revision,"summary":"Ready","changes":["fields.rs"],"to":"verify"}),
    )?;
    let h = b.post(
        "swarms/audit",
        "ada/impl",
        &handoff_body,
        &Default::default(),
    )?;
    assert_eq!(b.swarm_receipt("ada", "audit")?.handoffs.len(), 1);
    let verification = append_block(
        "",
        "verification",
        1,
        &json!({"handoff":h.id,"revision":revision,"summary":"Passed","checks":[{"name":"unit","status":"pass","command":"cargo test"}]}),
    )?;
    b.reply(h.id, "ada/verify", &verification, &Default::default())?;
    assert_eq!(b.swarm_receipt("ada", "audit")?.handoffs[0].status, "pass");
    let request = append_block(
        "",
        "decision-request",
        1,
        &json!({"to":"ada","summary":"Accept this revision?","handoff":h.id}),
    )?;
    let d = b.post("swarms/audit", "ada/verify", &request, &Default::default())?;
    assert!(
        b.attention("ada")?
            .iter()
            .any(|i| i.decision_id == Some(d.id) && i.action_required)
    );
    assert!(
        b.post("general", "ada/verify", &request, &Default::default())
            .is_err()
    );
    let resolved = append_block(
        "",
        "decision-resolution",
        1,
        &json!({"request":d.id,"summary":"Accepted"}),
    )?;
    b.reply(d.id, "ada", &resolved, &Default::default())?;
    assert!(b.swarm_receipt("ada", "audit")?.decisions.is_empty());
    Ok(())
}
