use board_local::{Board, Filter, PostOptions, sources::*};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{Arc, Barrier},
};

fn input(key: &str, t: &str, id: &str, p: Option<&str>, text: &str) -> Envelope {
    Envelope {
        key: key.into(),
        import: Import {
            thread: t.into(),
            external: id.into(),
            author: "42".into(),
            profile: None,
            parent: p.map(str::to_string),
            url: Some(format!("https://example.invalid/{t}/{id}")),
            updated_at: None,
            change: Change::Upsert {
                text: text.into(),
                labels: BTreeMap::new(),
            },
        },
        raw: json!({"fixture":key}),
    }
}
#[test]
fn incoming_replay_origin_edit_delete_and_offline_receipts_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board.db");
    let b = Board::open(&path).unwrap();
    b.create_source("ada", "tickets", Provider::Local, "tickets", "work")
        .unwrap();
    let root = input("one", "T", "root", None, "first");
    let q = b
        .enqueue_imports(
            "ada",
            "tickets",
            std::slice::from_ref(&root),
            Some(&Checkpoint {
                stream: "poll".into(),
                expected: None,
                next: "1".into(),
            }),
        )
        .unwrap();
    assert_eq!(
        q,
        b.enqueue_imports("ada", "tickets", std::slice::from_ref(&root), None)
            .unwrap()
    );
    drop(b);
    let b = Board::open(&path).unwrap();
    assert_eq!(
        b.source_checkpoint("ada", "tickets", "poll")
            .unwrap()
            .as_deref(),
        Some("1")
    );
    assert_eq!(b.drain_source("ada", "tickets", 50).unwrap(), (1, 0));
    let m = b.import_source("ada", "tickets", &root).unwrap();
    assert_eq!(m.actor, "source:tickets/42");
    let reply = input("reply", "T", "c1", Some("root"), "answer");
    let child = b.import_source("ada", "tickets", &reply).unwrap();
    assert_eq!(child.re, Some(m.id));
    let grand = b
        .import_source(
            "ada",
            "tickets",
            &input("grand", "T", "c2", Some("c1"), "to the answer"),
        )
        .unwrap();
    assert_eq!(grand.reply_to, Some(child.id));
    let mut e = input("changed", "T", "c1", Some("root"), "new answer");
    e.import.updated_at = Some("2024-02-01T00:00:00Z".parse().unwrap());
    b.import_source("ada", "tickets", &e).unwrap();
    e.key = "stale".into();
    e.import.updated_at = Some("2023-01-01T00:00:00Z".parse().unwrap());
    e.import.change = Change::Edit { text: "old".into() };
    b.import_source("ada", "tickets", &e).unwrap();
    assert_eq!(
        b.linked_source("ada", "tickets", "https://example.invalid/T/c1")
            .unwrap()
            .unwrap()
            .text,
        "new answer"
    );
    e.key = "delete".into();
    e.import.updated_at = None;
    e.import.change = Change::Delete;
    b.import_source("ada", "tickets", &e).unwrap();
    assert!(
        b.find_source("ada", "tickets", "T", Some("c1"))
            .unwrap()
            .unwrap()
            .deleted
    );
    assert!(b.find_source("other", "tickets", "T", None).is_err());
    assert_eq!(
        b.conversation(m.id, &Filter::default())
            .unwrap()
            .replies
            .len(),
        2
    );
    assert_eq!(
        b.source_copies("ada", "tickets", child.id).unwrap()[0].external,
        "c1"
    );
    let raw = b.inbox("ada", "tickets", Direction::In, 0, 2).unwrap();
    assert_eq!(raw[0].raw, json!({"fixture":"one"}));
    assert_eq!(raw[0].state, "done");
}
#[test]
fn competing_inbox_consumers_take_distinct_lanes_and_fence_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b");
    let b = Board::open(&path).unwrap();
    b.create_source("p", "local", Provider::Local, "stream", "jobs")
        .unwrap();
    let ids = b
        .enqueue_imports(
            "p",
            "local",
            &[
                input("A1", "A", "r", None, "a"),
                input("A2", "A", "r2", Some("r"), "b"),
                input("B1", "B", "r", None, "c"),
            ],
            None,
        )
        .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let mut leases = std::thread::scope(|t| {
        let mut hs = vec![];
        for _ in 0..2 {
            let path = path.clone();
            let bar = barrier.clone();
            hs.push(t.spawn(move || {
                let local = Board::open(path).unwrap();
                bar.wait();
                local
                    .lease_source("p", "local", Direction::In, 60)
                    .unwrap()
                    .unwrap()
            }));
        }
        hs.into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    leases.sort_by_key(|l| l.job.id);
    assert_eq!(leases[0].job.id, ids[0]);
    assert_eq!(leases[1].job.id, ids[2]);
    assert!(
        b.lease_source("p", "local", Direction::In, 60)
            .unwrap()
            .is_none()
    );
    assert!(
        b.finish_source("p", "local", ids[0], &leases[1].receipt, None)
            .is_err()
    );
    b.finish_source("p", "local", ids[0], &leases[0].receipt, None)
        .unwrap();
    assert_eq!(
        b.lease_source("p", "local", Direction::In, 60)
            .unwrap()
            .unwrap()
            .job
            .id,
        ids[1]
    );
}
#[test]
fn publishing_is_opt_in_and_ambiguous_remote_writes_cannot_be_silently_retried() {
    let b = Board::memory().unwrap();
    b.create_source("ada", "source", Provider::Local, "demo", "dev")
        .unwrap();
    let root = b
        .import_source(
            "ada",
            "source",
            &input("first", "T", "root", None, "from a provider"),
        )
        .unwrap();
    b.reply(root.id, "ada", "personal", &PostOptions::default())
        .unwrap();
    assert!(
        b.inbox("ada", "source", Direction::Out, 0, 10)
            .unwrap()
            .is_empty()
    );
    b.set_outbound("ada", "source", true).unwrap();
    let local = b
        .reply(root.id, "ada", "for provider", &PostOptions::default())
        .unwrap();
    b.import_source(
        "ada",
        "source",
        &input("in", "T", "external", Some("root"), "do not echo"),
    )
    .unwrap();
    let l = b
        .lease_source("ada", "source", Direction::Out, 45)
        .unwrap()
        .unwrap();
    assert_eq!(l.job.payload["message"], local.id);
    b.start_sending("ada", "source", l.job.id, &l.receipt)
        .unwrap();
    b.fail_source(
        "ada",
        "source",
        l.job.id,
        &l.receipt,
        FailureReport {
            reason: "response lost",
            uncertain: true,
            retry_after: None,
        },
    )
    .unwrap();
    b.reply(root.id, "ada", "next on same lane", &PostOptions::default())
        .unwrap();
    assert!(
        b.lease_source("ada", "source", Direction::Out, 45)
            .unwrap()
            .is_none()
    );
    b.resolve_source(
        "ada",
        "source",
        l.job.id,
        "delivered",
        Some(&DeliveryResult {
            external: "remote-99".into(),
            url: None,
            thread: None,
        }),
    )
    .unwrap();
    assert_eq!(
        b.find_source("ada", "source", "T", Some("remote-99"))
            .unwrap()
            .unwrap()
            .id,
        local.id
    );
    b.import_source(
        "ada",
        "source",
        &input("echo", "T", "remote-99", Some("root"), "platform echo"),
    )
    .unwrap();
    assert_eq!(b.get(local.id).unwrap().text, "for provider");
    assert!(
        b.lease_source("ada", "source", Direction::Out, 45)
            .unwrap()
            .is_some()
    );
    b.set_outbound("ada", "source", false).unwrap();
    assert!(
        b.lease_source("ada", "source", Direction::Out, 45)
            .unwrap()
            .is_none()
    );
}
#[test]
fn bad_input_cannot_advance_a_checkpoint_or_publish_half_a_batch() {
    let b = Board::memory().unwrap();
    b.create_source("p", "s", Provider::Local, "r", "ch")
        .unwrap();
    let good = input("first", "T", "r", None, "hi");
    let mut bad = good.clone();
    bad.import.parent = Some("r".into());
    assert!(
        b.enqueue_imports(
            "p",
            "s",
            &[good, bad],
            Some(&Checkpoint {
                stream: "cursor".into(),
                expected: None,
                next: "20".into()
            })
        )
        .is_err()
    );
    assert!(b.source_checkpoint("p", "s", "cursor").unwrap().is_none());
    assert!(b.inbox("p", "s", Direction::In, 0, 5).unwrap().is_empty());
    let key = input("bad", "T", "r", None, "");
    assert!(b.import_source("p", "s", &key).is_err());
    assert!(b.find_source("p", "s", "T", None).unwrap().is_none());
}
#[test]
fn disconnect_after_a_remote_send_requires_reconciliation_after_lease_expiry() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("board");
    let b = Board::open(&path).unwrap();
    b.create_source("p", "mirror", Provider::Local, "test", "dev")
        .unwrap();
    let root = b.post("dev", "p", "send", &Default::default()).unwrap();
    b.set_outbound("p", "mirror", true).unwrap();
    b.start_source_thread("p", "mirror", root.id).unwrap();
    let l = b
        .take_source_delivery("p", "mirror", Direction::Out, 1)
        .unwrap()
        .unwrap();
    drop(b);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let b = Board::open(&path).unwrap();
    assert!(
        b.take_source_delivery("p", "mirror", Direction::Out, 5)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        b.queue_item("p", "mirror", l.job.id).unwrap().state,
        "uncertain"
    );
    let confirm = DeliveryResult {
        thread: Some("t".into()),
        external: "r".into(),
        url: None,
    };
    assert!(
        b.finish_source("p", "mirror", l.job.id, &l.receipt, Some(&confirm))
            .is_err()
    );
    b.enqueue_imports(
        "p",
        "mirror",
        &[input("echo", "t", "r", None, "echo")],
        None,
    )
    .unwrap();
    assert_eq!(b.drain_source("p", "mirror", 10).unwrap(), (0, 0));
    b.resolve_source("p", "mirror", l.job.id, "delivered", Some(&confirm))
        .unwrap();
    assert_eq!(b.drain_source("p", "mirror", 10).unwrap(), (1, 0));
    assert_eq!(
        b.find_source("p", "mirror", "t", None).unwrap().unwrap().id,
        root.id
    );
}
