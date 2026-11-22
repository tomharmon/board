use board_local::{Board, Filter, PostOptions, sources::*};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{Arc, Barrier},
};

#[test]
fn registering_a_mapping_takes_destinations_not_provider_urls_or_bearers() -> anyhow::Result<()> {
    let b = Board::memory()?;
    let at = b.cursor()?;
    for (p, remote) in [
        (Provider::Github, "https://github.com/test/board"),
        (Provider::Slack, "xoxb-test-fixture"),
        (Provider::Linear, "lin_api_test-fixture"),
    ] {
        let e = b
            .create_source("p", "mapping", p, remote, "work")
            .unwrap_err();
        assert!(!e.to_string().contains(remote));
    }
    assert_eq!(b.cursor()?, at);
    assert!(b.sources("p")?.is_empty());
    b.create_source("p", "dm", Provider::Slack, "D012345", "work")?;
    b.create_source(
        "p",
        "local-events",
        Provider::Local,
        "arbitrary external stream/key",
        "work",
    )?;
    Ok(())
}

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
fn outbox_rechecks_an_edit_or_tombstone_between_leasing_and_sending() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("delivery.db");
    let worker = Board::open(&path)?;
    let writer = Board::open(&path)?;
    worker.create_source("p", "desk", Provider::Local, "external", "work")?;
    worker.set_outbound("p", "desk", true)?;
    let root = writer.post("work", "p", "root", &Default::default())?;
    worker.attach_source("p", "desk", "T", "external-root", root.id, None)?;
    let note = writer.reply(root.id, "p", "unsent draft", &Default::default())?;
    let lease = worker
        .lease_source("p", "desk", Direction::Out, 60)?
        .unwrap();
    writer.edit(note.id, "p", "approved text", None)?;
    assert_eq!(
        board_local::errors::classify(
            &worker
                .start_sending("p", "desk", lease.job.id, &lease.receipt)
                .unwrap_err()
        )
        .1,
        "delivery_changed"
    );
    let updated = worker
        .lease_source("p", "desk", Direction::Out, 60)?
        .unwrap();
    assert_eq!(updated.job.id, lease.job.id);
    assert_ne!(updated.receipt, lease.receipt);
    assert_eq!(updated.job.payload["text"], "approved text");
    writer.delete(note.id, "p", None)?;
    assert_eq!(
        board_local::errors::classify(
            &worker
                .start_sending("p", "desk", updated.job.id, &updated.receipt)
                .unwrap_err()
        )
        .1,
        "delivery_changed"
    );
    drop(worker);
    let worker = Board::open(&path)?;
    assert_eq!(
        worker.queue_item("p", "desk", lease.job.id)?.state,
        "skipped"
    );
    assert!(
        worker
            .take_source_delivery("p", "desk", Direction::Out, 60)?
            .is_none()
    );
    assert!(worker.source_copies("p", "desk", note.id)?.is_empty());
    Ok(())
}

#[test]
fn queued_platform_status_notes_keep_fenced_examples_as_prose() -> anyhow::Result<()> {
    let b = Board::memory()?;
    b.create_source("p", "desk", Provider::Local, "external", "work")?;
    for source in ["board", "desk"] {
        board_local::automation::add_watcher(
            &b,
            source,
            Filter {
                from: Some(source.to_owned()),
                by: Some("@[source:desk/42]".into()),
                ..Default::default()
            },
            Some("message.posted"),
        )?;
    }
    let head = input("1", "T", "root", None, "opened");
    let mut status = input("2", "T", "closed", Some("root"), "");
    let external = "```json#unpublished@1\nnot json\n```";
    status.import.change = Change::Signal {
        conditions: Default::default(),
        set: BTreeMap::from([("status".to_owned(), "closed".to_owned())]),
        unset: Vec::new(),
        reply: Some(external.into()),
    };
    b.enqueue_imports("p", "desk", &[head, status], None)?;
    assert_eq!(b.drain_source_report("p", "desk", 10)?.applied, 2);
    let root = b.find_source("p", "desk", "T", None)?.unwrap();
    let reply = b.find_source("p", "desk", "T", Some("closed"))?.unwrap();
    assert!(serde_json::to_value(&root)?.get("labels").is_none());
    assert_eq!(reply.re, Some(root.id));
    assert_eq!(reply.text, external);
    assert!(reply.blocks.is_empty());
    assert!(
        b.reply(root.id, "local-user", external, &Default::default())
            .is_err()
    );
    // Binding external origin replaces the first, provisional snapshot in the write.
    assert!(board_local::automation::next_event(&b, "board")?.is_none());
    for expected in [root.id, reply.id] {
        let e = board_local::automation::next_event(&b, "desk")?.unwrap();
        assert_eq!(e.message.unwrap().id, expected);
        board_local::automation::ack_event(&b, "desk", e.cursor)?;
    }
    Ok(())
}

#[test]
fn a_fresh_platform_edit_replaces_identical_local_data_with_prose_but_a_replay_does_not()
-> anyhow::Result<()> {
    let b = Board::memory()?;
    b.register_schema_as(
        "metric",
        1,
        "",
        &json!({"type":"object","properties":{"value":{"type":"integer"}}}),
        "admin",
    )?;
    b.create_source("p", "desk", Provider::Local, "external", "work")?;
    let seed = input("seed", "T", "head", None, "incoming");
    let root = b.import_source("p", "desk", &seed)?;
    let text = "```json#metric@1\n{\"value\":42}\n```";
    for (index, change) in [
        Change::Edit { text: text.into() },
        Change::Upsert {
            text: text.into(),
            labels: Default::default(),
        },
        Change::Reconcile {
            text: text.into(),
            labels: Default::default(),
            owns: vec![],
            reset: Default::default(),
            existing_only: true,
        },
    ]
    .into_iter()
    .enumerate()
    {
        let local = b.edit(root.id, &root.actor, text, None)?;
        assert_eq!(local.blocks.len(), 1);
        let observed = b.cursor()?;
        assert_eq!(
            b.import_source("p", "desk", &seed)?.revision,
            local.revision
        );
        assert_eq!(b.cursor()?, observed);
        let mut update = seed.clone();
        update.key = format!("change-{index}");
        update.import.change = change;
        let incoming = b.import_source("p", "desk", &update)?;
        assert!(
            incoming.blocks.is_empty(),
            "platform change {index} left a locally trusted block"
        );
        assert_eq!(incoming.text, text);
        assert!(incoming.revision > local.revision);
        let head = b.cursor()?;
        assert_eq!(
            b.import_source("p", "desk", &update)?.revision,
            incoming.revision
        );
        assert_eq!(b.cursor()?, head);
        update.key.push_str("-another-delivery");
        assert_eq!(
            b.import_source("p", "desk", &update)?.revision,
            incoming.revision
        );
    }
    Ok(())
}

#[test]
fn source_mentions_can_introduce_but_only_an_actors_own_writes_can_rename_them() {
    let b = Board::memory().unwrap();
    b.create_source("owner", "s", Provider::Local, "remote", "work")
        .unwrap();
    let mut root = input("root", "thread", "head", None, "Hello");
    root.import.author = "known-without-profile".into();
    b.import_source("owner", "s", &root).unwrap();
    let mut first = input(
        "intro",
        "thread",
        "mention",
        Some("head"),
        "@[source:s/new] and @[source:s/known-without-profile]",
    );
    first.import.mentioned.insert(
        "new".into(),
        AuthorInfo {
            name: "Alex".into(),
            avatar: None,
        },
    );
    first.import.mentioned.insert(
        "known-without-profile".into(),
        AuthorInfo {
            name: "Unexpected".into(),
            avatar: None,
        },
    );
    b.import_source("owner", "s", &first).unwrap();
    let get = |actor| {
        b.actor_directory(None)
            .unwrap()
            .into_iter()
            .find(|p| p.id == actor)
            .unwrap()
    };
    assert_eq!(get("source:s/new").name, "Alex");
    assert_ne!(get("source:s/known-without-profile").name, "Unexpected");
    let mut own = input("own", "thread", "own", Some("head"), "Here now");
    own.import.author = "new".into();
    own.import.profile = Some(AuthorInfo {
        name: "Alex Renamed".into(),
        avatar: Some("https://images.invalid/alex".into()),
    });
    b.import_source("owner", "s", &own).unwrap();
    let mut later = input(
        "another",
        "thread",
        "another",
        Some("head"),
        "@[source:s/new]",
    );
    later.import.mentioned.insert(
        "new".into(),
        AuthorInfo {
            name: "Not Alex".into(),
            avatar: Some("https://images.invalid/wrong".into()),
        },
    );
    let msg = b.import_source("owner", "s", &later).unwrap();
    assert_eq!(get("source:s/new").name, "Alex Renamed");
    assert_eq!(
        get("source:s/new").avatar.as_deref(),
        Some("https://images.invalid/alex")
    );
    let cursor = b.cursor().unwrap();
    assert_eq!(b.import_source("owner", "s", &later).unwrap().id, msg.id);
    assert_eq!(b.cursor().unwrap(), cursor);
}

#[test]
fn private_lifecycle_reply_is_never_mirrored_or_backfilled_and_sparse_profiles_keep_their_avatar() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sync.db");
    let b = Board::open(&path).unwrap();
    for name in ["upstream", "mirror"] {
        b.create_source("owner", name, Provider::Local, name, "ops")
            .unwrap();
    }
    let mut root = input("root", "a", "r", None, "Ready for review");
    root.import.profile = Some(AuthorInfo {
        name: "Dana".into(),
        avatar: Some("https://profile.invalid/dana.png".into()),
    });
    let id = b.import_source("owner", "upstream", &root).unwrap().id;
    b.attach_source("owner", "mirror", "second", "r", id, None)
        .unwrap();
    b.set_outbound("owner", "mirror", true).unwrap();
    let mut evt = input("lifecycle", "a", "closed", Some("r"), "");
    evt.import.profile = Some(AuthorInfo {
        name: "Dana Updated".into(),
        avatar: None,
    });
    evt.import.no_sync = true;
    evt.import.change = Change::Signal {
        conditions: Default::default(),
        set: [("state".into(), "closed".into())].into(),
        unset: vec![],
        reply: Some("Confidential review summary".into()),
    };
    let note = b.import_source("owner", "upstream", &evt).unwrap();
    assert!(note.no_sync);
    assert_eq!(note.re, Some(id));
    let card = b.get(note.id).unwrap().author.unwrap();
    assert_eq!(card.name, "Dana Updated");
    assert_eq!(
        card.avatar.as_deref(),
        Some("https://profile.invalid/dana.png")
    );
    let jobs = b.inbox("owner", "mirror", Direction::Out, 0, 50).unwrap();
    let state: String = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT value FROM labels WHERE message_id=? AND key='state'",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "closed");
    assert!(
        !jobs
            .iter()
            .any(|j| j.payload["message"] == id && j.payload["event"] == "message.edited"),
        "a source metadata change did not change the outbound text"
    );
    assert!(!jobs.iter().any(|j| j.payload["message"] == note.id));
    drop(b);
    let b = Board::open(&path).unwrap();
    assert_eq!(
        b.import_source("owner", "upstream", &evt).unwrap().id,
        note.id
    );
    b.backfill_source("owner", "mirror", "second").unwrap();
    assert!(
        !b.inbox("owner", "mirror", Direction::Out, 0, 50)
            .unwrap()
            .iter()
            .any(|j| j.payload["message"] == note.id)
    );
}

#[test]
fn read_only_copy_projection_resolves_all_readable_entries_without_exposing_raw_jobs_or_hidden_links()
 {
    use board_local::{access::Rules, errors::classify};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sync.db");
    let owner = Board::open(&path).unwrap();
    let build = |name, ch| {
        owner
            .create_source("operator", name, Provider::Local, name, ch)
            .unwrap();
        let root = owner
            .post(ch, "operator", "Thread", &Default::default())
            .unwrap();
        owner
            .attach_source(
                "operator",
                name,
                "r",
                "first",
                root.id,
                Some("https://local.invalid/shared"),
            )
            .unwrap();
        owner.set_outbound("operator", name, true).unwrap();
        root.id
    };
    let hidden = build("aaa", "private");
    let open = build("zzz", "public");
    let secret = owner
        .reply(hidden, "operator", "Private note", &Default::default())
        .unwrap();
    let msg = owner
        .reply(open, "operator", "Public note", &Default::default())
        .unwrap();
    owner
        .attach_source(
            "operator",
            "aaa",
            "r",
            "note",
            secret.id,
            Some("https://local.invalid/private"),
        )
        .unwrap();
    let receipt = owner
        .take_source_delivery("operator", "zzz", Direction::Out, 60)
        .unwrap()
        .unwrap();
    assert_eq!(
        owner.sync_targets(msg.id).unwrap().targets[0].state,
        "sending"
    );
    owner
        .finish_source(
            "operator",
            "zzz",
            receipt.job.id,
            &receipt.receipt,
            Some(&DeliveryResult {
                external: "note".into(),
                url: Some("https://local.invalid/visible".into()),
                thread: None,
            }),
        )
        .unwrap();
    let signed = owner
        .reply(open, "operator", "Signed-looking link", &Default::default())
        .unwrap();
    owner
        .attach_source(
            "operator",
            "zzz",
            "r",
            "signed",
            signed.id,
            Some("https://local.invalid/object?credential=DO_NOT_EXPOSE"),
        )
        .unwrap();
    owner
        .set_grant(
            "operator",
            None,
            "reader",
            &Rules::parse("public=read").unwrap(),
        )
        .unwrap();
    let token = owner
        .issue_credential("operator", "reader", None, None)
        .unwrap();
    let mut r = Board::open(&path).unwrap();
    r.bind_token(&token.token).unwrap();
    assert!(r.source("reader", "zzz").is_err());
    let found = r.locate_link("https://local.invalid/visible").unwrap();
    assert_eq!((found.message, found.root), (msg.id, open));
    assert_eq!(
        r.locate_link("https://local.invalid/shared").unwrap().root,
        open
    );
    let target = r.sync_targets(msg.id).unwrap();
    let line = &target.targets[0];
    assert_eq!(
        (target.root, target.message, target.cursor),
        (open, msg.id, found.cursor)
    );
    assert_eq!(
        (
            &line.source[..],
            line.external.as_deref(),
            line.url.as_deref(),
            &line.state[..]
        ),
        (
            "zzz",
            Some("note"),
            Some("https://local.invalid/visible"),
            "confirmed"
        )
    );
    assert!(r.sync_targets(signed.id).unwrap().targets[0].url.is_none());
    let private_note = owner
        .reply(
            open,
            "operator",
            "Only on this board",
            &PostOptions {
                no_sync: true,
                ..Default::default()
            },
        )
        .unwrap();
    let local = &r.sync_targets(private_note.id).unwrap().targets[0];
    assert_eq!(local.state, "local_only");
    assert_eq!(local.thread.as_deref(), Some("r"));
    assert!(local.url.is_none() && local.external.is_none());
    let payload = serde_json::to_string(&target).unwrap();
    for secret in [
        &*receipt.receipt,
        "DO_NOT_EXPOSE",
        "operator",
        "Public note",
        "aaa",
        "attempts",
    ] {
        assert!(!payload.contains(secret));
    }
    let errors = [
        "https://local.invalid/private",
        "https://local.invalid/unknown",
        "file:///tmp/unknown",
    ]
    .map(|url| r.locate_link(url).unwrap_err());
    for e in &errors {
        assert_eq!(classify(e).1, "message_not_found");
        assert_eq!(e.to_string(), errors[0].to_string());
    }
    assert_eq!(
        classify(&r.sync_targets(hidden).unwrap_err()).1,
        "message_not_found"
    );
    assert_eq!(
        classify(
            &owner
                .locate_link("https://local.invalid/shared")
                .unwrap_err()
        )
        .1,
        "ambiguous_link"
    );
}

#[test]
fn public_targets_do_not_confuse_queue_lane_names_with_remote_ids_or_remote_heads_with_local_replies()
 {
    let b = Board::memory().unwrap();
    b.create_source("owner", "gh", Provider::Github, "owner/repo", "dev")
        .unwrap();
    b.set_outbound("owner", "gh", true).unwrap();
    let (local, _) = b
        .post_with_targets(
            "dev",
            "owner",
            "Local note",
            &Default::default(),
            &["gh".into(), "gh:13".into()],
        )
        .unwrap();
    let targets = b.sync_targets(local.id).unwrap().targets;
    assert_eq!(targets.len(), 2);
    assert!(targets.iter().all(|t| t.external.is_none()));
    assert!(
        targets
            .iter()
            .any(|t| t.thread.is_none() && t.state == "pending")
    );
    assert!(
        targets
            .iter()
            .any(|t| t.thread.as_deref() == Some("13") && t.state == "not_sent")
    );
    let jobs = b.inbox("owner", "gh", Direction::Out, 0, 10).unwrap();
    assert_eq!(jobs.len(), 1, "start queues the root; join never does");
    assert_eq!(jobs[0].payload["event"], "thread.start");
    let url = "https://github.com/owner/repo/issues/13";
    assert_eq!(b.locate_link(url).unwrap().message, local.id);
    let mut head = input("head", "13", "issue", None, "Original provider issue");
    head.import.url = Some(url.into());
    let remote = b.import_source("owner", "gh", &head).unwrap();
    assert_eq!(remote.re, Some(local.id));
    let located = b.locate_link(url).unwrap();
    assert_eq!((located.message, located.root), (remote.id, local.id));
}

#[test]
fn compatible_provider_permalinks_resolve_offline_only_when_their_location_matches() {
    let b = Board::memory().unwrap();
    b.create_source("owner", "gh", Provider::Github, "owner/repo", "dev")
        .unwrap();
    let root = b
        .post("dev", "owner", "Review", &Default::default())
        .unwrap();
    let note = b
        .reply(root.id, "owner", "Line 4", &Default::default())
        .unwrap();
    b.attach_source(
        "owner",
        "gh",
        "12",
        "issue",
        root.id,
        Some("https://github.com/owner/repo/pull/12"),
    )
    .unwrap();
    b.attach_source(
        "owner",
        "gh",
        "12",
        "inline:300",
        note.id,
        Some("https://github.com/owner/repo/pull/12#discussion_r300"),
    )
    .unwrap();
    assert_eq!(
        b.locate_link("https://github.com/OWNER/REPO/pull/12/files#discussion_r300")
            .unwrap()
            .message,
        note.id
    );
    assert!(
        b.locate_link("https://github.com/owner/repo/pull/12/files#discussion_r999")
            .is_err()
    );
    assert!(
        b.locate_link("https://github.com/lost/repo/pull/12#discussion_r300")
            .is_err()
    );
    b.create_source("owner", "chat", Provider::Slack, "C1", "dev")
        .unwrap();
    b.attach_source(
        "owner",
        "chat",
        "1700000000.010000",
        "1700000000.010000",
        root.id,
        Some("https://one.slack.com/archives/C1/p1700000000010000"),
    )
    .unwrap();
    b.attach_source(
        "owner",
        "chat",
        "1700000000.010000",
        "1700000001.020000",
        note.id,
        None,
    )
    .unwrap();
    let link =
        "https://one.slack.com/archives/C1/p1700000001020000?thread_ts=1700000000.010000&cid=C1";
    assert_eq!(b.locate_link(link).unwrap().message, note.id);
    assert!(b.locate_link(&link.replace("one.", "wrong.")).is_err());
    assert!(
        b.locate_link(&link.replace("thread_ts=1700000000.010000", "thread_ts=1700000009.010000"))
            .is_err()
    );
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
    e.import.profile = Some(AuthorInfo {
        name: "Current Name".into(),
        avatar: None,
    });
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
    e.import.profile = Some(AuthorInfo {
        name: "Stale Name".into(),
        avatar: Some("https://old.invalid/picture".into()),
    });
    e.import.updated_at = Some("2024-01-01T00:00:00Z".parse().unwrap());
    e.import.links[0].text = Some("stale quote".into());
    e.raw = json!({"files":[{"id":"F1","name":"obsolete","size":12}]});
    b.import_source("p", "s", &e).unwrap();
    let author = b.get(m.id).unwrap().author.unwrap();
    assert_eq!(author.name, "Current Name");
    assert!(author.avatar.is_none());
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
