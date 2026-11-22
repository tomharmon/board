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
    let failed = b.attention("ada")?;
    assert_eq!(
        failed
            .iter()
            .map(|i| i.waiting.as_str())
            .collect::<Vec<_>>(),
        ["ada/verify", "ada/review"]
    );
    assert!(
        failed
            .iter()
            .all(|i| i.kind == "launch-failed" && i.action_required && i.assigned_to == "ada")
    );
    assert!(b.attention("ada/impl")?.iter().all(|i| !i.action_required));
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
    let dependent = create_task(
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
    let why = b.swarm_explain_task("ada", "audit", dependent.id, 10)?;
    assert_eq!(
        why.steps[0].waits_on,
        [other.id],
        "pending review is not a queue dependency"
    );
    assert!(why.steps.iter().all(|s| s.id != a.id));
    let completed = b.swarm_explain_task("ada", "audit", a.id, 1)?;
    assert_eq!(completed.steps[0].state, "completed");
    assert_eq!(completed.steps[0].handoff.as_ref().unwrap().to, "verify");
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
fn task_explanation_follows_only_visible_unfinished_edges_and_current_revision() -> Result<()> {
    use board_local::access::Rules;
    use board_local::automation::{TaskTarget, create_task};
    let mut b = board()?;
    let opts = PostOptions::default();
    let task = |ch, text, needs: &[i64]| {
        create_task(
            &b,
            "ada",
            TaskTarget::New {
                channel: ch,
                text,
                options: &opts,
            },
            needs,
            None,
        )
    };
    let run = "swarms/audit";
    let behind_hidden = task("shared", "Reachable only through a private task", &[])?;
    let hidden = task("private", "Private gate", &[behind_hidden.id])?;
    let outside = task("shared", "Shared input", &[hidden.id])?;
    let leaf = task(run, "Package", &[])?;
    let middle = task(run, "Release", &[leaf.id])?;
    let root = task(run, "Publish", &[middle.id, outside.id])?;
    let revision = json!({"kind":"sha256","value":"a".repeat(64)});
    let handoff = |r: &Value| {
        post(
            &b,
            "ada/impl",
            "handoff",
            json!({"task":leaf.id,
        "revision":r, "summary":"Packaged","changes":["archive"],"to":"verify"}),
        )
    };
    let old = handoff(&revision)?;
    post(
        &b,
        "ada/verify",
        "verification",
        json!({"handoff":old,"revision":revision,
        "summary":"Checked","checks":[{"name":"unit","status":"pass","command":"test"}]}),
    )?;
    let current = handoff(&json!({"kind":"sha256","value":"b".repeat(64)}))?;
    let other = b.swarm_start(
        "ada",
        &SwarmStart {
            slug: Some("other-run".into()),
            goal: "Independent review".into(),
            ..Default::default()
        },
    )?;
    b.swarm_add_worker(
        "ada",
        &other.slug,
        &SwarmAddWorker {
            name: "guard".into(),
            brief: "Own review".into(),
            ..Default::default()
        },
    )?;
    b.swarm_post("ada", &other.slug, &append_block("", "handoff", 1, &json!({
        "task":outside.id,"revision":revision,"summary":"Other run only","changes":["note"],"to":"guard"
    }))?, &Default::default())?;
    b.set_grant(
        "ada",
        None,
        "guest",
        &Rules::parse("swarms/=read;shared=read")?,
    )?;
    let token = b.issue_credential("ada", "guest", None, None)?.token;
    b.bind_token(&token)?;
    let cursor = b.cursor()?;
    let why = b.swarm_explain_task("guest", "audit", root.id, 4)?;
    assert_eq!(
        why.steps.iter().map(|s| s.id).collect::<Vec<_>>(),
        [root.id, outside.id, middle.id, leaf.id]
    );
    assert_eq!(why.steps[0].waits_on, [outside.id, middle.id]);
    assert!(why.steps[1].waits_on.is_empty());
    assert_eq!(
        why.steps[1].reason,
        "No unfinished prerequisite is visible."
    );
    assert!(
        why.steps[1].handoff.is_none(),
        "do not import other runs' evidence"
    );
    assert_eq!(why.steps[2].waits_on, [leaf.id]);
    let last = &why.steps[3];
    assert_eq!(last.state, "ready");
    assert!(last.reason.contains("no reason for inactivity"));
    let evidence = last.handoff.as_ref().unwrap();
    assert_eq!((evidence.id, evidence.to.as_str()), (current, "verify"));
    assert!(evidence.verification.is_none());
    assert!(!why.truncated);
    let short = b.swarm_explain_task("guest", "audit", root.id, 1)?;
    assert!(short.truncated);
    assert_eq!(short.steps[0].waits_on, [outside.id, middle.id]);
    assert!(
        b.swarm_explain_task("guest", "audit", outside.id, 5)
            .is_err()
    );
    assert_eq!(b.cursor()?, cursor);
    Ok(())
}

#[test]
fn task_inputs_follow_the_latest_readable_revision_at_open_and_claim() -> Result<()> {
    use board_local::access::Rules;
    use board_local::automation::{
        ClaimTarget, TaskAction, TaskTarget, change_task, claim, create_task,
    };
    let mut b = board()?;
    let opts = PostOptions::default();
    let task = |channel, text, needs: &[i64]| {
        create_task(
            &b,
            "ada",
            TaskTarget::New {
                channel,
                text,
                options: &opts,
            },
            needs,
            None,
        )
    };
    let input = task("public", "Produce artifact", &[])?;
    let private = task("internal", "Independent private check", &[])?;
    let dependent = task("public", "Use both results", &[input.id, private.id])?;
    let acquire = |id| claim(&b, "ada", ClaimTarget::Id(id), 120, &Default::default());
    for (id, result) in [
        (input.id, "artifact output"),
        (private.id, "sensitive output"),
    ] {
        let t = acquire(id)?;
        change_task(
            &b,
            id,
            "ada",
            Some(t.epoch),
            t.claim_token.as_deref(),
            TaskAction::Complete(Some(result)),
        )?;
    }
    let publish = |value: &str| {
        post(
            &b,
            "ada/impl",
            "handoff",
            json!({
                "task":input.id,"to":"verify","summary":"Ready","changes":["artifact"],
                "revision":{"kind":"sha256","value":value.repeat(64)}
            }),
        )
    };
    let check = |handoff, value: &str, status| {
        post(
            &b,
            "ada/verify",
            "verification",
            json!({
                "handoff":handoff,"revision":{"kind":"sha256","value":value.repeat(64)},"summary":"Checked",
                "checks":[{"name":"artifact","command":"test artifact","status":status}]
            }),
        )
    };
    let open = |b: &Board| -> Result<_> {
        Ok(b.conversation(dependent.id, &Default::default())?
            .message
            .task
            .unwrap())
    };
    let first = publish("a")?;
    let failed = check(first, "a", "fail")?;
    let claimed = acquire(dependent.id)?;
    assert_eq!(claimed.inputs, open(&b)?.inputs);
    assert_eq!(claimed.inputs.len(), 2);
    assert_eq!(claimed.inputs[0].id, input.id);
    assert_eq!(claimed.inputs[0].state, "completed");
    assert_eq!(claimed.inputs[0].result.as_deref(), Some("artifact output"));
    let verification = claimed.inputs[0]
        .handoff
        .as_ref()
        .unwrap()
        .verification
        .as_ref()
        .unwrap();
    assert_eq!(
        (verification.id, verification.status.as_str()),
        (failed, "fail")
    );
    assert!(claimed.inputs[1].handoff.is_none());
    let next = publish("b")?;
    // Even a late pass on A cannot verify B.
    check(first, "a", "pass")?;
    let latest = open(&b)?.inputs.remove(0).handoff.unwrap();
    assert_eq!((latest.id, latest.revision.value), (next, "b".repeat(64)));
    assert!(latest.verification.is_none());
    let not_run = check(next, "b", "not-run")?;
    let check = open(&b)?
        .inputs
        .remove(0)
        .handoff
        .unwrap()
        .verification
        .unwrap();
    assert_eq!((check.id, check.status.as_str()), (not_run, "not-run"));
    b.set_grant("ada", None, "guest", &Rules::parse("public=read")?)?;
    let token = b.issue_credential("ada", "guest", None, None)?.token;
    b.bind_token(&token)?;
    let scoped = open(&b)?;
    assert_eq!(scoped.needs, [input.id]);
    assert_eq!(scoped.inputs.len(), 1);
    assert_eq!(scoped.inputs[0].result.as_deref(), Some("artifact output"));
    assert!(
        scoped.inputs[0].handoff.is_none(),
        "a readable task does not grant access to its verifier's channel"
    );
    Ok(())
}

#[test]
fn ordinary_task_overview_pages_and_links_the_latest_exact_evidence() -> Result<()> {
    use board_local::automation::{TaskTarget, create_task};
    let b = board()?;
    let mut opts = PostOptions::default();
    opts.labels.insert("workstream".into(), "Storage".into());
    let task = |channel, text| {
        create_task(
            &b,
            "ada",
            TaskTarget::New {
                channel,
                text,
                options: &opts,
            },
            &[],
            Some("ada/impl"),
        )
    };
    let first = task(
        "swarms/audit",
        "Verify migration\nLong details stay in the thread",
    )?;
    task("elsewhere", "Foreign queue")?;
    let last = task("swarms/audit", "Check restore")?;
    let revision = json!({"kind":"sha256","value":"a".repeat(64)});
    let publish = |r: &Value| {
        post(
            &b,
            "ada/impl",
            "handoff",
            json!({"task":first.id,"revision":r,
        "summary":"Ready","changes":["storage.rs"],"to":"verify"}),
        )
    };
    let handoff = publish(&revision)?;
    let check = post(
        &b,
        "ada/verify",
        "verification",
        json!({"handoff":handoff,"revision":revision,"summary":"Tested",
        "checks":[{"name":"restore","status":"pass","command":"test restore"}]}),
    )?;
    let view = b.swarm_observe("ada", "audit", None, 0, 1)?;
    assert_eq!(view.next_task_after, Some(first.id));
    assert_eq!(view.goal, "Verify a revision");
    let thread = &view.threads[0];
    assert_eq!(
        (
            &*thread.title,
            thread.workstream.as_deref(),
            thread.assignee.as_deref()
        ),
        ("Verify migration", Some("Storage"), Some("ada/impl"))
    );
    assert_eq!(
        (thread.latest_handoff, thread.latest_check),
        (Some(handoff), Some(check))
    );
    let next = b.swarm_observe_page("ada", "audit", None, 0, first.id, 1)?;
    assert_eq!(next.threads[0].id, last.id);
    assert!(next.next_task_after.is_none());
    let newer = publish(&json!({"kind":"sha256","value":"b".repeat(64)}))?;
    let thread = &b.swarm_observe("ada", "audit", None, 0, 1)?.threads[0];
    assert_eq!(
        (thread.latest_handoff, thread.latest_check),
        (Some(newer), None)
    );
    Ok(())
}

#[test]
fn public_bookmarks_and_addressed_filters_do_not_consume_another_inbox() -> Result<()> {
    let b = board()?;
    let msg = b
        .swarm_post("ada", "audit", "@verify review", &Default::default())?
        .message
        .id;
    b.swarm_dm("ada", "audit", "impl", "Different recipient")?;
    let dm = b.swarm_dm("ada", "audit", "verify", "Private question")?;
    let before = b.swarm_status("ada", "audit")?.workers;
    b.swarm_mark_public_seen("ada", "audit", msg)?;
    assert_eq!(b.swarm_public_seen("ada", "audit")?, msg);
    assert_eq!(b.swarm_public_seen("ada/verify", "audit")?, 0);
    assert!(
        b.swarm_mark_public_seen("ada", "audit", dm.event_cursor)
            .is_err()
    );
    let entries = b
        .swarm_read_filtered(
            "ada/verify",
            "audit",
            msg - 1,
            10,
            false,
            &SwarmFilter {
                kind: Some("addressed".into()),
                worker: None,
            },
        )?
        .entries;
    assert_eq!(
        entries.iter().map(|e| e.cursor).collect::<Vec<_>>(),
        [msg, dm.event_cursor]
    );
    let after = b.swarm_status("ada", "audit")?.workers;
    assert_eq!(before[1].last_read_cursor, after[1].last_read_cursor);
    b.swarm_set_limit("ada", "audit", 0)?;
    let handoff = post(
        &b,
        "ada/impl",
        "handoff",
        json!({"revision":{"kind":"sha256","value":"a".repeat(64)},
        "summary":"Quiet handoff","changes":["artifact"],"to":"verify"}),
    )?;
    let entries = b
        .swarm_read_filtered(
            "ada/verify",
            "audit",
            dm.event_cursor,
            10,
            false,
            &SwarmFilter {
                kind: Some("addressed".into()),
                worker: None,
            },
        )?
        .entries;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].cursor, handoff);
    assert!(entries[0].notified.is_empty());
    let member_view = b.swarm_read_filtered(
        "ada",
        "audit",
        dm.event_cursor,
        10,
        true,
        &SwarmFilter {
            kind: Some("public".into()),
            worker: Some("verify".into()),
        },
    )?;
    assert_eq!(member_view.entries[0].cursor, handoff);
    Ok(())
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
    let inbox = b.attention("ada")?;
    assert_eq!(inbox.iter().filter(|i| i.action_required).count(), 1);
    assert!(inbox.iter().any(|i| {
        i.kind == "verification"
            && !i.action_required
            && i.waiting == "ada/verify"
            && i.assigned_to == "ada/impl"
            && snapshot
                .actions
                .iter()
                .any(|a| a.message_id == i.message_id && a.summary == i.summary)
    }));
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
    assert!(
        b.attention("ada")?.is_empty(),
        "an unverified handoff should not alert the operator"
    );
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
    rusqlite::Connection::open(&path)?
        .execute_batch("DROP TABLE swarm_policy; PRAGMA user_version=17;")?;
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
fn v18_swarm_plan_tasks_and_batch_receipts_survive_ledger_removal() -> Result<()> {
    use board_local::automation::{create_plan, lookup_plan};
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("previous");
    let b = Board::open(&path)?;
    let run = b.swarm_start_with_policy(
        "ada",
        &SwarmStart {
            goal: "Keep existing tasks".into(),
            ..Default::default()
        },
        RunPolicy {
            max_active: 3,
            retry_budget: 1,
        },
    )?;
    let key = format!("swarm-plan:{}:original", run.id);
    let items = serde_json::from_value::<Vec<_>>(json!([
        {"key":"one","text":"Original task","labels":{"swarm.plan":"original","swarm.item":"one"},"no_sync":true}
    ]))?;
    let tasks = create_plan(&b, "ada", &run.channel, &items, Some(&key))?;
    let head = b.cursor()?;
    drop(b);

    let previous = rusqlite::Connection::open(&path)?;
    previous.execute_batch(
        "CREATE TABLE swarm_plans (run_id INTEGER NOT NULL REFERENCES swarm_runs(id),
          key TEXT NOT NULL, actor TEXT NOT NULL, digest BLOB NOT NULL, PRIMARY KEY (run_id,key));
          PRAGMA user_version=18;",
    )?;
    previous.execute(
        "INSERT INTO swarm_plans VALUES (?,'original','ada',zeroblob(32))",
        [run.id],
    )?;
    let upgraded = Board::open(&path)?;
    assert_eq!(lookup_plan(&upgraded, "ada", &key)?, tasks);
    assert_eq!(
        upgraded
            .swarm_observe("ada", &run.slug, None, 0, 1)?
            .policy
            .max_active,
        3
    );
    assert_eq!(upgraded.cursor()?, head);
    assert!(!previous.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='swarm_plans')",
        [],
        |row| row.get::<_, bool>(0)
    )?);
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
