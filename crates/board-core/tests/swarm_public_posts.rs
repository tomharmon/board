use anyhow::Result;
use board_local::{
    Board, ComposerSettings, Draft, PostOptions,
    access::Rules,
    swarm::{SwarmAddWorker, SwarmStart, WaitPoll},
};

#[test]
fn public_roots_replies_and_desktop_composer_share_one_delivery_budget() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = Board::open(dir.path().join("board"))?;
    let run = db.swarm_start(
        "ada",
        &SwarmStart {
            goal: "Check the patch".into(),
            slug: Some("test".into()),
            max_notifications: Some(1),
            ..Default::default()
        },
    )?;
    for name in ["impl", "verify"] {
        db.swarm_add_worker(
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
    let first = db.post(
        &run.channel,
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
    let scope = format!("thread:{}", first.id);
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
    assert_eq!((reply.re, reply.reply_to), (Some(first.id), Some(quiet.id)));
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
            slug: Some("test".into()),
            ..Default::default()
        },
    )?;
    owner.swarm_add_worker(
        "ada",
        "test",
        &SwarmAddWorker {
            name: "verify".into(),
            brief: "Verify".into(),
            ..Default::default()
        },
    )?;
    owner.set_grant("ada", None, "guest", &Rules::parse("swarms/test=post")?)?;
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
