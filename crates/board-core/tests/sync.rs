use board_local::{Board, Filter, PostOptions, sources::*};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{Arc, Barrier},
};

#[test]
fn disabling_publishing_and_files_fences_leases_but_permits_an_explicit_unsent_backfill() {
    let b = Board::memory().unwrap();
    b.create_source("p", "s", Provider::Slack, "C1", "work")
        .unwrap();
    b.patch_source(
        "p",
        "s",
        &SourcePatch {
            outbound: Some(true),
            mirror_files: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    let root = b.post("work", "p", "root", &Default::default()).unwrap();
    b.attach_source("p", "s", "t", "r", root.id, None).unwrap();
    let reply = b
        .reply(
            root.id,
            "p",
            "note",
            &PostOptions {
                files: vec![("n.txt".into(), b"data".to_vec())],
                ..Default::default()
            },
        )
        .unwrap();
    let old = b
        .lease_source("p", "s", Direction::Out, 15)
        .unwrap()
        .unwrap();
    b.set_outbound("p", "s", false).unwrap();
    assert!(b.start_sending("p", "s", old.job.id, &old.receipt).is_err());
    b.set_outbound("p", "s", true).unwrap();
    assert!(
        b.lease_source("p", "s", Direction::Out, 15)
            .unwrap()
            .is_none()
    );
    assert_eq!(b.backfill_source("p", "s", "t").unwrap(), 1);
    assert_eq!(b.backfill_source("p", "s", "t").unwrap(), 0);
    let l = b
        .take_source_delivery("p", "s", Direction::Out, 15)
        .unwrap()
        .unwrap();
    b.finish_source(
        "p",
        "s",
        l.job.id,
        &l.receipt,
        Some(&DeliveryResult {
            external: "c".into(),
            thread: None,
            url: None,
        }),
    )
    .unwrap();
    let f = b
        .lease_source("p", "s", Direction::Out, 15)
        .unwrap()
        .unwrap();
    assert_eq!(f.job.payload["event"], "file.share");
    b.patch_source(
        "p",
        "s",
        &SourcePatch {
            mirror_files: Some(false),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(b.start_sending("p", "s", f.job.id, &f.receipt).is_err());
    assert!(
        b.lease_source("p", "s", Direction::Out, 15)
            .unwrap()
            .is_none()
    );
    let fresh = b
        .stage_source_file("p", "s", "t", reply.id, reply.attachments[0].id)
        .unwrap();
    assert_eq!(fresh.id, f.job.id);
    assert_eq!(fresh.state, "pending");
}

fn input(key: &str, t: &str, id: &str, p: Option<&str>, text: &str) -> Envelope {
    Envelope {
        key: key.into(),
        import: Import {
            links: Vec::new(),
            no_sync: false,
            mentioned: Default::default(),
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
fn independent_mirrors_forward_imports_not_echoes_and_never_export_private_threads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board");
    let b = Board::open(&path).unwrap();
    for s in ["one", "two"] {
        b.create_source("p", s, Provider::Local, s, "dev").unwrap();
        b.patch_source(
            "p",
            s,
            &SourcePatch {
                outbound: Some(true),
                mirror_roots: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let root = b
        .import_source("p", "one", &input("root", "A", "root", None, "from one"))
        .unwrap();
    let child = b
        .import_source(
            "p",
            "one",
            &input("child", "A", "reply", Some("root"), "child"),
        )
        .unwrap();
    assert!(
        b.import_source(
            "p",
            "one",
            &input("wrong", "A", "another", None, "second root")
        )
        .is_err()
    );
    let hidden = b
        .post(
            "dev",
            "p",
            "private",
            &PostOptions {
                no_sync: true,
                ..Default::default()
            },
        )
        .unwrap();
    b.reply(hidden.id, "p", "also private", &Default::default())
        .unwrap();
    let secret = b
        .reply(
            root.id,
            "p",
            "private reply",
            &PostOptions {
                no_sync: true,
                ..Default::default()
            },
        )
        .unwrap();
    for s in ["one", "two"] {
        assert!(
            b.attach_source("p", s, "other", "x", hidden.id, None)
                .is_err()
        );
        assert!(b.start_source_thread("p", s, hidden.id).is_err());
    }
    drop(b);
    let b = Board::open(&path).unwrap();
    assert!(b.get(secret.id).unwrap().no_sync);
    assert!(
        b.inbox("p", "one", Direction::Out, 0, 30)
            .unwrap()
            .is_empty()
    );
    let jobs = b.inbox("p", "two", Direction::Out, 0, 30).unwrap();
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].payload["event"], "thread.start");
    assert_eq!(jobs[1].payload["message"], child.id);
    let l = b
        .take_source_delivery("p", "two", Direction::Out, 30)
        .unwrap()
        .unwrap();
    b.finish_source(
        "p",
        "two",
        l.job.id,
        &l.receipt,
        Some(&DeliveryResult {
            thread: Some("B".into()),
            external: "copyroot".into(),
            url: None,
        }),
    )
    .unwrap();
    assert_eq!(b.backfill_source("p", "two", "B").unwrap(), 0);
    b.import_source(
        "p",
        "two",
        &input("echo", "B", "copyroot", None, "rendered by the provider"),
    )
    .unwrap();
    let l = b
        .take_source_delivery("p", "two", Direction::Out, 30)
        .unwrap()
        .unwrap();
    assert_eq!(l.job.thread, "B");
    b.finish_source(
        "p",
        "two",
        l.job.id,
        &l.receipt,
        Some(&DeliveryResult {
            thread: None,
            external: "copyreply".into(),
            url: None,
        }),
    )
    .unwrap();
    assert_eq!(b.get(root.id).unwrap().text, "from one");
    assert!(
        b.inbox("p", "one", Direction::Out, 0, 30)
            .unwrap()
            .is_empty()
    );
    let mut private = input("private", "Z", "root", None, "private external");
    private.import.no_sync = true;
    let m = b.import_source("p", "one", &private).unwrap();
    assert!(m.no_sync);
    assert!(
        b.take_source_delivery("p", "two", Direction::Out, 30)
            .unwrap()
            .is_none()
    );
}
#[test]
fn outbox_reads_current_text_coalesces_edits_and_skips_an_unsent_tombstone() {
    let b = Board::memory().unwrap();
    b.create_source("p", "s", Provider::Local, "one", "dev")
        .unwrap();
    let r = b.post("dev", "p", "root", &Default::default()).unwrap();
    b.set_outbound("p", "s", true).unwrap();
    b.attach_source("p", "s", "T", "r", r.id, None).unwrap();
    let a = b.reply(r.id, "p", "v1", &Default::default()).unwrap();
    b.edit(a.id, "p", "v2", None).unwrap();
    let d = b.reply(r.id, "p", "not sent", &Default::default()).unwrap();
    b.delete(d.id, "p", None).unwrap();
    let l = b
        .take_source_delivery("p", "s", Direction::Out, 30)
        .unwrap()
        .unwrap();
    assert_eq!(l.job.payload["text"], "v2");
    b.finish_source(
        "p",
        "s",
        l.job.id,
        &l.receipt,
        Some(&DeliveryResult {
            thread: None,
            external: "c".into(),
            url: None,
        }),
    )
    .unwrap();
    assert!(
        b.take_source_delivery("p", "s", Direction::Out, 30)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        b.inbox("p", "s", Direction::Out, 0, 10)
            .unwrap()
            .iter()
            .filter(|j| j.state == "skipped")
            .count(),
        3
    );
}
#[test]
fn stored_link_quotes_are_same_channel_snapshots_and_never_a_blind_read_or_network_fetch() {
    use board_local::access::Rules;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board");
    let b = Board::open(&path).unwrap();
    b.create_source("p", "sl", Provider::Slack, "C1", "private")
        .unwrap();
    let link = "https://example.slack.com/archives/C1/p1234567890000001";
    let mut ext = input(
        "key",
        "1234567890.000001",
        "1234567890.000001",
        None,
        "original quote",
    );
    ext.import.url = Some(link.into());
    let r = b.import_source("p", "sl", &ext).unwrap();
    let same = b.post("private", "p", link, &Default::default()).unwrap();
    assert_eq!(same.links[0].text.as_deref(), Some("original quote"));
    assert_eq!(same.links[0].message, Some(r.id));
    let other = b.post("public", "p", link, &Default::default()).unwrap();
    assert!(other.links[0].message.is_none() && other.links[0].text.is_none());
    let code = b
        .post("private", "p", &format!("`{link}`"), &Default::default())
        .unwrap();
    assert!(code.links.is_empty());
    ext.key = "edit".into();
    ext.import.change = Change::Edit {
        text: "new wording".into(),
    };
    b.import_source("p", "sl", &ext).unwrap();
    assert_eq!(
        b.get(same.id).unwrap().links[0].text.as_deref(),
        Some("original quote")
    );
    b.set_grant("p", None, "writer", &Rules::parse("private=post").unwrap())
        .unwrap();
    let tok = b.issue_credential("p", "writer", None, None).unwrap().token;
    let mut blind = Board::open(&path).unwrap();
    blind.bind_token(&tok).unwrap();
    let opts = PostOptions {
        key: Some("blind-link".into()),
        ..Default::default()
    };
    assert!(
        blind
            .post("private", "writer", link, &opts)
            .unwrap()
            .links
            .is_empty()
    );
    assert!(
        blind
            .posted("writer", "blind-link")
            .unwrap()
            .links
            .is_empty()
    );
}
#[test]
fn source_renderer_rereads_current_labels_only_updates_changed_text_and_fences_a_failed_start() {
    let b = Board::memory().unwrap();
    b.create_source("p", "s", Provider::Local, "other", "dev")
        .unwrap();
    b.patch_source(
        "p",
        "s",
        &SourcePatch {
            outbound: Some(true),
            mirror_roots: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    b.set_source_renderer(
        "p",
        "s",
        Some(r#"message.text+" ["+(message.labels.phase ?? "open")+"]""#),
    )
    .unwrap();
    let m = b.post("dev", "p", "ticket", &Default::default()).unwrap();
    let l = b
        .take_source_delivery("p", "s", Direction::Out, 10)
        .unwrap()
        .unwrap();
    assert_eq!(l.job.payload["text"], "ticket [open]");
    b.ack_source(
        "p",
        "s",
        l.job.id,
        &l.receipt,
        Some(&DeliveryResult {
            external: "x".into(),
            thread: Some("T".into()),
            url: None,
        }),
    )
    .unwrap();
    b.label(m.id, "p", "unrelated", Some("x")).unwrap();
    assert!(
        b.take_source_delivery("p", "s", Direction::Out, 10)
            .unwrap()
            .is_none()
    );
    b.label(m.id, "p", "phase", Some("ready")).unwrap();
    let l = b
        .take_source_delivery("p", "s", Direction::Out, 10)
        .unwrap()
        .unwrap();
    assert_eq!(l.job.payload["text"], "ticket [ready]");
    b.ack_source(
        "p",
        "s",
        l.job.id,
        &l.receipt,
        Some(&DeliveryResult {
            external: "x".into(),
            thread: None,
            url: None,
        }),
    )
    .unwrap();
    b.set_source_renderer("p", "s", Some("loop {}")).unwrap();
    let other = b.post("dev", "p", "new", &Default::default()).unwrap();
    b.reply(other.id, "p", "queued child", &Default::default())
        .unwrap();
    assert!(
        b.take_source_delivery("p", "s", Direction::Out, 10)
            .unwrap()
            .is_none()
    );
    let failed = b.message_deliveries("p", other.id).unwrap();
    assert_eq!(failed[0].state, "failed");
    assert!(b.message_deliveries("other", other.id).unwrap().is_empty());
    b.resolve_source("p", "s", failed[0].id, "skip", None)
        .unwrap();
    b.set_source_renderer("p", "s", None).unwrap();
    let retry = b.start_source_thread("p", "s", other.id).unwrap();
    assert_ne!(failed[0].id, retry.id);
    let new = b
        .take_source_delivery("p", "s", Direction::Out, 10)
        .unwrap()
        .unwrap();
    assert_eq!(new.job.id, retry.id);
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
fn a_failed_inbound_head_fences_only_its_lane_until_reconciled() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board");
    let b = Board::open(&path).unwrap();
    b.create_source("p", "s", Provider::Local, "feed", "dev")
        .unwrap();
    let ids = b
        .enqueue_imports(
            "p",
            "s",
            &[
                input("bad", "a", "bad", None, "bad"),
                input("later", "a", "root", None, "next root"),
            ],
            None,
        )
        .unwrap();
    let clock = rusqlite::Connection::open(&path).unwrap();
    for n in 1..=10 {
        let l = b
            .lease_source("p", "s", Direction::In, 30)
            .unwrap()
            .unwrap();
        assert_eq!(l.job.id, ids[0]);
        let job = b
            .fail_source(
                "p",
                "s",
                ids[0],
                &l.receipt,
                FailureReport {
                    reason: "cannot normalize",
                    uncertain: false,
                    retry_after: Some(1),
                },
            )
            .unwrap();
        assert_eq!(job.state, if n == 10 { "failed" } else { "pending" });
        // Advance only the retry clock, not the queue state or the receipt.
        clock
            .execute("UPDATE sync_queue SET available=0 WHERE id=?", [ids[0]])
            .unwrap();
    }
    assert!(
        b.lease_source("p", "s", Direction::In, 30)
            .unwrap()
            .is_none()
    );
    b.enqueue_imports(
        "p",
        "s",
        &[input("other", "b", "root", None, "independent")],
        None,
    )
    .unwrap();
    assert_eq!(b.drain_source("p", "s", 10).unwrap(), (1, 0));
    drop(b);
    let b = Board::open(&path).unwrap();
    b.resolve_source("p", "s", ids[0], "skip", None).unwrap();
    assert_eq!(b.drain_source("p", "s", 10).unwrap(), (1, 0));
    assert_eq!(
        b.find_source("p", "s", "a", None).unwrap().unwrap().text,
        "next root"
    );
}
#[test]
fn an_older_import_cannot_replace_a_newer_external_quote_or_enqueue_its_files() {
    use board_local::links::Link;
    let b = Board::memory().unwrap();
    b.create_source("p", "s", Provider::Slack, "C1", "work")
        .unwrap();
    b.patch_source(
        "p",
        "s",
        &SourcePatch {
            mirror_files: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    let url = "https://demo.slack.com/archives/C2/p1234567890000001";
    let mut e = input("first", "T", "r", None, url);
    e.import.updated_at = Some("2025-01-01T00:00:00Z".parse().unwrap());
    e.import.links = vec![Link {
        url: url.into(),
        message: None,
        author: Some("Ada".into()),
        text: Some("latest quote".into()),
        reason: None,
    }];
    let m = b.import_source("p", "s", &e).unwrap();
    assert_eq!(m.links[0].text.as_deref(), Some("latest quote"));
    e.key = "stale".into();
    e.import.updated_at = Some("2024-01-01T00:00:00Z".parse().unwrap());
    e.import.links[0].text = Some("stale quote".into());
    e.raw = json!({"files":[{"id":"F1","name":"obsolete","size":12}]});
    b.import_source("p", "s", &e).unwrap();
    assert_eq!(
        b.get(m.id).unwrap().links[0].text.as_deref(),
        Some("latest quote")
    );
    assert!(b.pending_media("p", "s", false).unwrap().is_empty());
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
