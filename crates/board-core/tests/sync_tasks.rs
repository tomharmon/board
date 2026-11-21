use anyhow::Result;
use board_local::{
    Board,
    access::Rules,
    automation::{self, ClaimTarget, TaskAction},
    sources::{Direction, Envelope, Provider},
};
use serde_json::json;

fn issue(thread: &str, external: &str, parent: Option<&str>) -> Envelope {
    serde_json::from_value(json!({"key":format!("{thread}-{external}"),"import":{
        "thread":thread,"external":external,"parent":parent,"author":"reporter",
        "change":{"op":"upsert","text":external,"labels":{"tag.bug":"1","state":"open"}}
    }}))
    .unwrap()
}

#[test]
fn task_intake_survives_restart_and_never_promotes_replies_or_resets_existing_work() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let path = tmp.path().join("board");
    let b = Board::open(&path)?;
    b.create_source("ada", "bugs", Provider::Local, "ticket archive", "issues")?;
    let older = issue("old", "root", None);
    let ordinary = b.import_source("ada", "bugs", &older)?;
    let root = issue("new", "root", None);
    let reply = issue("new", "comment", Some("root"));
    let batch = [older, root, reply];
    b.enqueue_imports_with_tasks("ada", "bugs", &batch, None, true)?;
    drop(b);
    let b = Board::open(&path)?;
    assert_eq!(b.drain_source("ada", "bugs", 10)?, (2, 0));
    let m = b.find_source("ada", "bugs", "new", None)?.unwrap();
    let task = m.task.as_ref().unwrap();
    assert_eq!((&*task.state, &*task.by), ("ready", "ada"));
    assert_eq!(m.labels["tag.bug"], "1");
    assert_eq!(m.labels["state"], "open");
    assert!(b.get(ordinary.id)?.task.is_none());
    let r = b
        .find_source("ada", "bugs", "new", Some("comment"))?
        .unwrap();
    assert_eq!(r.re, Some(m.id));
    assert!(r.task.is_none());
    let lease = automation::claim(
        &b,
        "ada/worker",
        ClaimTarget::Id(m.id),
        300,
        &Default::default(),
    )?;
    b.enqueue_imports_with_tasks("ada", "bugs", &batch, None, true)?;
    assert_eq!(b.drain_source("ada", "bugs", 10)?, (0, 0));
    assert_eq!(automation::get_task(&b, m.id)?.epoch, lease.epoch);
    automation::change_task(
        &b,
        m.id,
        "ada/worker",
        Some(lease.epoch),
        lease.claim_token.as_deref(),
        TaskAction::Complete(Some("done")),
    )?;
    let mut update = batch[1].clone();
    update.key = "new title".into();
    b.enqueue_imports_with_tasks("ada", "bugs", &[update], None, true)?;
    assert_eq!(b.drain_source("ada", "bugs", 10)?, (1, 0));
    let t = automation::get_task(&b, m.id)?;
    assert_eq!(t.state, "completed");
    assert_eq!(t.result.as_deref(), Some("done"));
    Ok(())
}

#[test]
fn failed_promotion_commits_no_root_mapping_or_receipt_and_recovery_promotes_once() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("board");
    let b = Board::open(&path)?;
    b.create_source("ada", "bugs", Provider::Local, "tickets", "issues")?;
    let id = b.enqueue_imports_with_tasks("ada", "bugs", &[issue("a", "r", None)], None, true)?[0];
    let c = rusqlite::Connection::open(&path)?;
    c.execute_batch("CREATE TRIGGER disk_fault BEFORE INSERT ON tasks BEGIN SELECT RAISE(ABORT,'test interruption');END;")?;
    let leased = b.lease_source("ada", "bugs", Direction::In, 30)?.unwrap();
    assert!(
        b.finish_source("ada", "bugs", id, &leased.receipt, None)
            .is_err()
    );
    assert!(b.find_source("ada", "bugs", "a", None)?.is_none());
    assert_eq!(b.queue_item("ada", "bugs", id)?.state, "leased");
    drop(b);
    c.execute_batch("DROP TRIGGER disk_fault;")?;
    let b = Board::open(&path)?;
    let m = b.finish_source("ada", "bugs", id, &leased.receipt, None)?;
    assert_eq!(m.task.unwrap().state, "ready");
    assert_eq!(b.queue_item("ada", "bugs", id)?.result, Some(m.id));
    assert!(
        b.finish_source("ada", "bugs", id, &leased.receipt, None)
            .is_err()
    );
    Ok(())
}

#[test]
fn a_different_drainer_cannot_resurrect_the_pullers_revoked_privileges() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("board");
    let b = Board::open(&path)?;
    b.create_source("ops", "bugs", Provider::Local, "tickets", "issues")?;
    let grant = b.set_grant("owner", None, "ops", &Rules::parse("issues=read,sync")?)?;
    let token = b.issue_credential("owner", "ops", None, None)?;
    let mut scoped = Board::open(&path)?;
    scoped.bind_token(&token.token)?;
    let item = issue("a", "r", None);
    assert!(
        scoped
            .enqueue_imports_with_tasks("ops", "bugs", std::slice::from_ref(&item), None, true)
            .is_err()
    );
    assert!(b.inbox("ops", "bugs", Direction::In, 0, 20)?.is_empty());
    let ordinary = b.enqueue_imports("ops", "bugs", &[issue("ordinary", "r", None)], None)?[0];
    assert!(
        scoped
            .drain_source_report_with_tasks("ops", "bugs", 10, true)
            .is_err()
    );
    assert_eq!(b.queue_item("ops", "bugs", ordinary)?.attempts, 0);
    b.set_grant(
        "owner",
        Some(grant.id),
        "ops",
        &Rules::parse("issues=read,sync,label")?,
    )?;
    scoped.enqueue_imports_with_tasks("ops", "bugs", &[item], None, true)?;
    b.revoke_credential("owner", &token.credential.id)?;
    let report = b.drain_source_report_with_tasks("ops", "bugs", 10, true)?;
    assert_eq!((report.applied, report.failed), (1, 1));
    assert_eq!(
        b.find_source("ops", "bugs", "ordinary", None)?
            .unwrap()
            .task
            .unwrap()
            .by,
        "ops"
    );
    assert!(b.find_source("ops", "bugs", "a", None)?.is_none());
    Ok(())
}
