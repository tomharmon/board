use anyhow::Result;
use board_local::{
    Board, ComposerSettings, Draft, PostOptions,
    access::Rules,
    swarm::{SwarmAddWorker, SwarmStart, WaitPoll},
};

#[test]
fn public_replies_and_desktop_composer_share_one_delivery_budget() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = Board::open(dir.path().join("board"))?;
    let run = db.swarm_start(
        "ada",
        &SwarmStart {
            goal: "Check the patch".into(),
            channel: Some("engineering".into()),
            slug: Some("test".into()),
            max_notifications: Some(1),
            ..Default::default()
        },
    )?;
    for name in ["impl", "verify"] {
        db.swarm_register_worker(
            "ada",
            "test",
            &SwarmAddWorker {
                name: name.into(),
                brief: name.into(),
                ..Default::default()
            },
        )?;
    }
    let key = PostOptions {
        key: Some("public-root".into()),
        ..Default::default()
    };
    let unrelated = db.post(&run.channel, "ada/impl", "@verify FYI", &Default::default())?;
    assert!(unrelated.notified.is_empty());
    let first = db.reply(
        run.thread_id,
        "ada/impl",
        "@verify @verify please check",
        &key,
    )?;
    assert_eq!(first.notified, ["verify"]);
    let replay = db.swarm_post("ada/impl", "test", "@impl changed on replay", &key)?;
    assert_eq!(replay.message.id, first.id);
    assert_eq!(replay.notified, ["verify"]);
    // The single peer delivery has spent the budget. A reply still lands, but does not wake.
    let reply_key = PostOptions {
        key: Some("public-reply".into()),
        ..Default::default()
    };
    let quiet = db.reply(
        first.id,
        "ada/impl",
        "@verify please check again; @coordinator FYI",
        &reply_key,
    )?;
    assert_eq!(quiet.notified, ["coordinator"]);
    assert_eq!(quiet.suppressed, ["verify"]);
    db.swarm_set_limit("ada", "test", 2)?;
    let cursor = db.cursor()?;
    let replay = db.reply(first.id, "ada/impl", "different text", &reply_key)?;
    assert_eq!(replay.suppressed, quiet.suppressed);
    assert_eq!(replay.notified, quiet.notified);
    let entry = db
        .swarm_read("ada", "test", quiet.id - 1, 1, false)?
        .entries
        .remove(0);
    assert_eq!(entry.notified, quiet.notified);
    assert_eq!(entry.suppressed, quiet.suppressed);
    assert_eq!(db.cursor()?, cursor);
    let draft = Draft {
        body: "Human follow-up @verify".into(),
        channel: run.channel.clone(),
        reply_to: Some(quiet.id),
    };
    let scope = format!("thread:{}", run.thread_id);
    let stamp = db
        .save_composer(
            "ada",
            &scope,
            None,
            Some((&draft, &ComposerSettings::default())),
            None,
        )?
        .unwrap();
    let reply = db.reply_composer(
        quiet.id,
        "ada",
        &draft.body,
        &Default::default(),
        (&scope, &stamp),
    )?;
    assert_eq!(
        (reply.re, reply.reply_to),
        (Some(run.thread_id), Some(quiet.id))
    );
    assert_eq!(reply.text, "Human follow-up @[ada/verify]");
    assert!(db.load_composer("ada", &scope)?.is_none());
    let WaitPoll::Ready { messages, .. } = db.swarm_poll_wait("ada/verify", "test", "verify", 0)?
    else {
        panic!("lost verifier messages")
    };
    assert_eq!(
        messages.iter().map(|m| m.id).collect::<Vec<_>>(),
        [first.id, reply.id]
    );
    assert_eq!(db.swarm_status("ada", "test")?.notifications_sent, 1);
    db.swarm_stop("ada", "test")?;
    let archived = db.reply(
        first.id,
        "ada",
        "@coordinator archived note",
        &Default::default(),
    )?;
    let feed = db.swarm_read("ada", "test", archived.id - 1, 1, false)?;
    assert!(feed.entries[0].notified.is_empty());
    Ok(())
}

#[test]
fn blind_writer_does_not_resolve_a_swarm_roster_or_notify_workers() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("board");
    let owner = Board::open(&file)?;
    let run = owner.swarm_start(
        "ada",
        &SwarmStart {
            goal: "Review".into(),
            channel: Some("engineering".into()),
            slug: Some("test".into()),
            ..Default::default()
        },
    )?;
    owner.swarm_register_worker(
        "ada",
        "test",
        &SwarmAddWorker {
            name: "verify".into(),
            brief: "Verify".into(),
            ..Default::default()
        },
    )?;
    owner.set_grant("ada", None, "guest", &Rules::parse("engineering=post")?)?;
    let mut guest = Board::open(&file)?;
    guest.bind_token(&owner.issue_credential("ada", "guest", None, None)?.token)?;
    let post = guest.post(&run.channel, "guest", "@verify hello", &Default::default())?;
    assert_eq!(post.text, "@verify hello");
    assert!(post.notified.is_empty() && post.suppressed.is_empty());
    assert!(
        guest
            .reply(post.id, "guest", "again", &Default::default())
            .is_err()
    );
    assert!(matches!(
        owner.swarm_poll_wait("ada/verify", "test", "verify", 0)?,
        WaitPoll::Pending
    ));
    Ok(())
}

#[test]
fn linked_work_does_not_copy_claim_or_grant_access_to_a_thread() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("board");
    let b = Board::open(&path)?;
    let opts = PostOptions::default();
    let anchor = b.post("team", "ada", "Original conversation", &opts)?;
    let outside = b.post("team", "ada", "Unrelated conversation", &opts)?;
    let work = b.post("restricted", "ada", "Original investigation", &opts)?;
    let context = b.reply(work.id, "ada", "Evidence from before the swarm", &opts)?;
    let spec = SwarmStart {
        thread_id: Some(anchor.id),
        goal: "Ship the fix".into(),
        slug: Some("ship".into()),
        ..Default::default()
    };
    let run = b.swarm_start("ada", &spec)?;
    assert_eq!((run.channel.as_str(), run.thread_id), ("team", anchor.id));
    assert_eq!(b.get(anchor.id)?.text, anchor.text);
    assert_eq!(b.swarm_start("ada", &spec)?.id, run.id);
    assert!(
        b.swarm_start(
            "ada",
            &SwarmStart {
                slug: None,
                thread_id: Some(context.id),
                ..spec.clone()
            }
        )
        .is_err()
    );
    assert!(
        b.swarm_start(
            "ada",
            &SwarmStart {
                thread_id: None,
                slug: None,
                ..spec.clone()
            }
        )
        .is_err()
    );
    let reg = b.swarm_register_worker(
        "ada",
        "ship",
        &SwarmAddWorker {
            name: "test".into(),
            brief: "Check the findings".into(),
            thread_id: Some(work.id),
            ..Default::default()
        },
    )?;
    assert_eq!(reg.worker.thread_id, Some(work.id));
    assert!(b.get(work.id)?.task.is_none());
    let notified =
        b.swarm_post_to_thread("ada", "ship", Some(context.id), "@test please check", &opts)?;
    assert_eq!(notified.notified, ["test"]);
    assert_eq!(notified.message.reply_to, Some(context.id));
    assert!(
        b.swarm_post_to_thread("ada", "ship", Some(outside.id), "Not linked", &opts)
            .is_err()
    );
    let ids = |b: &Board, who: &str| -> Result<Vec<i64>> {
        Ok(b.swarm_read(who, "ship", 0, 30, false)?
            .entries
            .into_iter()
            .filter_map(|e| e.message.map(|m| m.id))
            .collect())
    };
    let full = ids(&b, "ada")?;
    assert!(full.contains(&work.id) && full.contains(&context.id) && !full.contains(&outside.id));
    b.set_grant("ada", None, "ada/test", &Rules::parse("team=read,post")?)?;
    let mut limited = Board::open(&path)?;
    limited.bind_token(&b.issue_credential("ada", "ada/test", None, None)?.token)?;
    let binding = limited.worker_binding("ship/test")?;
    let assigned = limited.worker_assignment(&binding)?;
    assert_eq!(assigned.thread_id, anchor.id);
    assert_eq!(assigned.worker.thread_id, None);
    assert_eq!(
        limited.swarm_status("ada/test", "ship")?.workers[0].thread_id,
        None
    );
    let visible = ids(&limited, "ada/test")?;
    assert!(!visible.contains(&work.id) && !visible.contains(&notified.message.id));
    assert_eq!(
        limited.swarm_status("ada/test", "ship")?.activity_count,
        visible.len() as i64
    );
    assert!(matches!(
        limited.swarm_poll_wait("ada/test", "ship", "test", 0)?,
        WaitPoll::Pending
    ));
    assert!(
        limited
            .swarm_post_to_thread("ada/test", "ship", Some(work.id), "invisible", &opts)
            .is_err()
    );
    // The opposite grant can see the work but not the swarm's budget or roster.
    b.swarm_set_limit("ada", "ship", 0)?;
    let suppressed =
        b.swarm_post_to_thread("ada/author", "ship", Some(work.id), "@test review", &opts)?;
    assert_eq!(suppressed.suppressed, ["test"]);
    b.set_grant("ada", None, "reader", &Rules::parse("restricted=read")?)?;
    let mut work_only = Board::open(&path)?;
    work_only.bind_token(&b.issue_credential("ada", "reader", None, None)?.token)?;
    assert!(work_only.get(suppressed.message.id)?.suppressed.is_empty());
    assert!(
        work_only
            .event_feed(
                0,
                100,
                &Default::default(),
                Some("swarm.notification_suppressed"),
                true
            )?
            .1
            .is_empty()
    );
    b.swarm_start(
        "ada",
        &SwarmStart {
            slug: Some("other".into()),
            ..spec
        },
    )?;
    b.swarm_register_worker(
        "ada",
        "other",
        &SwarmAddWorker {
            name: "test".into(),
            brief: "Second opinion".into(),
            thread_id: Some(work.id),
            ..Default::default()
        },
    )?;
    assert!(
        b.reply(work.id, "ada", "@test ambiguous", &opts)?
            .notified
            .is_empty()
    );
    let explicit =
        b.swarm_post_to_thread("ada", "other", Some(work.id), "@test explicit", &opts)?;
    assert_eq!(explicit.notified, ["test"]);
    let ship = b.swarm_read("ada", "ship", explicit.message.id - 1, 1, false)?;
    assert_eq!(ship.entries[0].cursor, explicit.message.id);
    assert!(ship.entries[0].notified.is_empty());
    assert!(
        b.reply(anchor.id, "ada", "@test another ambiguous reply", &opts)?
            .notified
            .is_empty()
    );
    assert_eq!(
        b.swarm_post("ada", "ship", "@test intended swarm", &opts)?
            .notified,
        ["test"]
    );
    b.swarm_stop("ada", "other")?;
    assert_eq!(b.swarm_status("ada", "ship")?.status, "active");
    assert_eq!(
        b.reply(anchor.id, "ada", "@test uniquely linked now", &opts)?
            .notified,
        ["test"]
    );
    Ok(())
}

#[test]
fn a_key_replays_the_original_publication_not_a_different_swarms_delivery() -> Result<()> {
    let b = Board::memory()?;
    let root = b.post("team", "ada", "Shared work", &Default::default())?;
    let start = |slug: &str| {
        b.swarm_start(
            "ada",
            &SwarmStart {
                goal: slug.into(),
                slug: Some(slug.into()),
                thread_id: Some(root.id),
                ..Default::default()
            },
        )
    };
    start("one")?;
    b.swarm_register_worker(
        "ada",
        "one",
        &SwarmAddWorker {
            name: "test".into(),
            brief: "Review".into(),
            ..Default::default()
        },
    )?;
    let keyed = |key: &str| PostOptions {
        key: Some(key.into()),
        ..Default::default()
    };
    let receipt = b.reply(root.id, "ada", "@test check", &keyed("notify"))?;
    let plain = b.swarm_post("ada", "one", "No mention", &keyed("plain"))?;
    start("two")?;
    let cursor = b.cursor()?;
    for key in ["notify", "plain"] {
        let err = b
            .swarm_post("ada", "two", "retry", &keyed(key))
            .unwrap_err();
        assert_eq!(board_local::errors::classify(&err).1, "duplicate_id");
    }
    assert_eq!(
        b.swarm_post("ada", "one", "retry", &keyed("plain"))?
            .message
            .id,
        plain.message.id
    );
    assert_eq!(
        b.reply(root.id, "ada", "retry", &keyed("notify"))?.notified,
        ["test"]
    );
    assert_eq!(b.cursor()?, cursor);
    b.swarm_stop("ada", "one")?;
    let retry = b.reply(
        root.id,
        "ada",
        "retry after routing changed",
        &keyed("notify"),
    )?;
    assert_eq!(retry.id, receipt.id);
    assert_eq!(retry.notified, ["test"]);
    assert!(
        b.swarm_read("ada", "two", receipt.id - 1, 1, false)?
            .entries[0]
            .notified
            .is_empty()
    );
    Ok(())
}
