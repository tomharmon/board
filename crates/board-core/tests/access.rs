use anyhow::Result;
use board_local::{
    Board, Filter, PostOptions,
    access::{GrantSpec, Rules},
    errors::{Kind, classify},
};

#[test]
fn grant_batches_are_atomic_replayable_and_owner_only() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("policy.sqlite3");
    let b = Board::open(&path)?;
    let sql = rusqlite::Connection::open(&path)?;
    let grants = [
        GrantSpec {
            subject: "agent".into(),
            can: Rules::parse("*=read")?,
        },
        GrantSpec {
            subject: "helper".into(),
            can: Rules::parse("ops/=post")?,
        },
    ];
    sql.execute_batch(
        "CREATE TRIGGER stop_second BEFORE INSERT ON access_grants
        WHEN NEW.subject='helper' BEGIN SELECT RAISE(ABORT,'rejected second grant'); END;",
    )?;
    let before = b.cursor()?;
    assert!(
        b.create_grant_batch("owner", &grants, Some("policy-1"))
            .is_err()
    );
    assert_eq!(b.cursor()?, before);
    assert!(b.grants()?.is_empty());
    sql.execute_batch("DROP TRIGGER stop_second;")?;
    for size in [0, 17] {
        let bad = vec![grants[0].clone(); size];
        assert_eq!(
            classify(&b.create_grant_batch("owner", &bad, None).unwrap_err()).1,
            "invalid_batch"
        );
    }
    let mut bad = grants.clone();
    bad[1].subject = "/missing-principal".into();
    assert!(b.create_grant_batch("owner", &bad, None).is_err());
    assert_eq!(b.cursor()?, before);
    assert!(b.grants()?.is_empty());
    let barrier = std::sync::Barrier::new(2);
    let attempt = || -> Result<_> {
        let db = Board::open(&path)?;
        barrier.wait();
        db.create_grant_batch("owner", &grants, Some("policy-1"))
    };
    let ((batch, a), (same, z)) = std::thread::scope(|s| -> Result<_> {
        let t = s.spawn(attempt);
        let second = attempt()?;
        Ok((t.join().unwrap()?, second))
    })?;
    assert_ne!(a, z);
    assert_eq!(batch, same);
    assert_eq!(batch.grants, b.grants()?);
    assert_eq!(
        b.events(before, 10)?
            .iter()
            .map(|e| e.kind.as_str())
            .collect::<Vec<_>>(),
        ["grant.changed", "grant.changed", "grant.batch_created"]
    );
    assert_eq!(
        classify(
            &b.create_grant_batch("owner", &grants[..1], Some("policy-1"))
                .unwrap_err()
        )
        .0,
        Kind::Conflict
    );
    assert_eq!(
        classify(
            &b.create_grant_batch("other", &grants, Some("policy-1"))
                .unwrap_err()
        )
        .0,
        Kind::Conflict
    );
    assert_eq!(b.cursor()?, batch.id);
    let secret = b.issue_credential("owner", "agent", None, None)?.token;
    let mut scoped = Board::open(&path)?;
    scoped.bind_token(&secret)?;
    assert!(scoped.events(before, 10)?.is_empty());
    assert!(scoped.grant_batch(batch.id).is_err());
    assert!(scoped.create_grant_batch("agent", &grants, None).is_err());
    b.revoke_grant("owner", batch.grants[0].id)?;
    b.set_grant(
        "owner",
        Some(batch.grants[1].id),
        "changed",
        &Rules::parse("read")?,
    )?;
    let reopened = Board::open(&path)?;
    assert_eq!(reopened.grant_batch(batch.id)?, batch);
    assert_eq!(
        reopened.create_grant_batch("owner", &grants, Some("policy-1"))?,
        (batch, false)
    );
    assert_eq!(b.grants()?.len(), 1);
    Ok(())
}

#[test]
fn channel_rules_union_per_grant_intersect_caps_and_refresh_live() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("access.sqlite3");
    let mut b = Board::open(&path)?;
    let grant = b.set_grant(
        "owner",
        None,
        "ada",
        &Rules::parse("*=read;dev/=read,post;dev/closed/=none;dev/exact=react")?,
    )?;
    b.set_grant(
        "owner",
        None,
        "ada/bot",
        &Rules::parse("dev/closed/public=label")?,
    )?;
    let issued = b.issue_credential("owner", "ada/bot", Some(&Rules::parse("dev/=all")?), None)?;
    let hidden = b.post(
        "private",
        "owner",
        "not visible",
        &PostOptions {
            files: vec![("private.bin".into(), vec![9])],
            ..Default::default()
        },
    )?;
    let denied = b.post("dev/closed", "owner", "also private", &Default::default())?;
    let visible = b.post("dev/open", "reader", "visible", &Default::default())?;
    let exact = b.post("dev/exact", "someone", "exact", &Default::default())?;
    let exception = b.post(
        "dev/closed/public",
        "reader",
        "visible exception",
        &Default::default(),
    )?;
    let mut limited = Board::open(&path)?;
    assert_eq!(limited.bind_token(&issued.token)?, "ada/bot");
    let before = b.cursor()?;
    assert_eq!(
        classify(&limited.get(hidden.id).unwrap_err()),
        (Kind::Missing, "message_not_found")
    );
    assert_eq!(
        classify(&limited.get(denied.id).unwrap_err()).0,
        Kind::Missing
    );
    assert_eq!(
        classify(&limited.get(i64::MAX).unwrap_err()).0,
        Kind::Missing
    );
    assert!(limited.channel("private").is_err());
    assert!(limited.conversation(denied.id, &Filter::default()).is_err());
    assert!(limited.download(hidden.attachments[0].id).is_err());
    assert!(!limited.actors()?.contains(&"owner".to_string()));
    assert_eq!(
        limited
            .messages(&Filter::default(), 100)?
            .iter()
            .map(|m| m.id)
            .collect::<Vec<_>>(),
        [visible.id, exact.id, exception.id]
    );
    assert_eq!(limited.threads(&Filter::default(), 100)?.len(), 3);
    assert_eq!(limited.channel_page("", None, 2)?.1.len(), 2);
    assert_eq!(limited.events(0, 100)?.len(), 3);
    assert_eq!(limited.event_tail(100, &Filter::default())?.1.len(), 3);
    assert!(
        limited
            .reply(exact.id, "ada/bot", "no post", &Default::default())
            .is_err()
    );
    assert!(
        limited
            .reply(denied.id, "ada/bot", "leak", &Default::default())
            .is_err()
    );
    assert!(
        limited
            .post("dev/open", "owner", "spoof", &Default::default())
            .is_err()
    );
    assert!(
        limited
            .post(
                "dev/open",
                "ada/bot",
                "with labels",
                &PostOptions {
                    labels: [("a".into(), "x".into())].into(),
                    ..Default::default()
                }
            )
            .is_err()
    );
    assert!(
        limited
            .label(visible.id, "ada/bot", "state", Some("done"))
            .is_err()
    );
    assert!(limited.backup(&dir.path().join("no-copy")).is_err());
    assert!(limited.grants().is_err() && limited.credentials().is_err());
    assert!(
        limited
            .issue_credential("owner", "root", None, None)
            .is_err()
    );
    assert_eq!(b.cursor()?, before);
    limited.react(exact.id, "ada/bot", "eyes", false)?;
    limited.label(exception.id, "ada/bot", "state", Some("done"))?;
    limited.reply(visible.id, "ada/bot", "mine", &Default::default())?;
    b.set_grant(
        "owner",
        Some(grant.id),
        "ada",
        &Rules::parse("dev/exact=read")?,
    )?;
    assert!(limited.get(visible.id).is_err());
    assert!(limited.react(exact.id, "ada/bot", "heart", false).is_err());
    assert!(limited.get(exception.id).is_ok());
    assert!(
        b.with_request(
            Some(&issued.token),
            "owner",
            Some("private=all"),
            |db, actor| db.post("private", actor, "can't widen", &Default::default())
        )
        .is_err()
    );
    let secret = &issued.token;
    assert!(!serde_json::to_string(&b.credentials()?)?.contains(secret));
    assert!(!serde_json::to_string(&b.events(0, 200)?)?.contains(secret));
    b.revoke_credential("owner", &issued.credential.id)?;
    assert_eq!(
        classify(&limited.get(exact.id).unwrap_err()).1,
        "invalid_credential"
    );
    assert!(Board::open(path)?.bind_token(secret).is_err());
    Ok(())
}

#[test]
fn blind_own_retries_return_only_the_original_post_not_thread_activity() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("blind.sqlite");
    let b = Board::open(&path)?;
    let grant = b.set_grant("owner", None, "runner", &Rules::parse("in/=post")?)?;
    let secret = b.issue_credential("owner", "runner", None, None)?.token;
    let mut agent = Board::open(&path)?;
    agent.bind_token(&secret)?;
    assert!(agent.create_channel_as("in/log", "runner").is_err());
    let key = PostOptions {
        key: Some("ping".into()),
        ..Default::default()
    };
    let m = agent.post("in/log", "runner", "original", &key)?;
    b.react(m.id, "secret-person", "eyes", false)?;
    b.label(m.id, "owner", "internal", Some("confidential"))?;
    b.edit(m.id, "runner", "internal edit", None)?;
    assert_eq!(agent.posted("runner", "ping")?, m);
    assert_eq!(agent.post("in/log", "runner", "different", &key)?, m);
    assert!(
        agent.get(m.id).is_err()
            && agent
                .reply(m.id, "runner", "no guessing", &Default::default())
                .is_err()
    );
    assert!(agent.channels()?.is_empty());
    assert!(
        agent
            .post(
                "in/log",
                "runner",
                "no labels",
                &PostOptions {
                    labels: [("tag".into(), "x".into())].into(),
                    ..Default::default()
                }
            )
            .is_err()
    );
    let fixed = agent.edit(m.id, "runner", "fixed", None)?;
    assert!(fixed.labels.is_empty() && fixed.reactions.is_empty());
    b.set_grant(
        "owner",
        Some(grant.id),
        "runner",
        &Rules::parse("in/=post,read")?,
    )?;
    let second = agent.post(
        "in/log",
        "runner",
        "written with read",
        &PostOptions {
            key: Some("second".into()),
            ..Default::default()
        },
    )?;
    assert_eq!(agent.get(m.id)?.text, "fixed");
    b.set_grant(
        "owner",
        Some(grant.id),
        "runner",
        &Rules::parse("in/=post")?,
    )?;
    assert!(agent.posted("runner", "second").is_err());
    assert!(agent.edit(second.id, "runner", "hidden", None).is_err());
    assert!(agent.delete(second.id, "runner", None).is_err());
    Ok(())
}

#[test]
fn historical_tail_uses_current_content_but_live_resume_keeps_event_time_and_checkpoints() {
    let b = Board::memory().unwrap();
    let m = b
        .post("dev", "ann", "old words", &Default::default())
        .unwrap();
    b.react(m.id, "bob", "eyes", false).unwrap();
    b.label(m.id, "ann", "one", Some("removed")).unwrap();
    b.edit(m.id, "ann", "new words", None).unwrap();
    b.react(m.id, "bob", "eyes", true).unwrap();
    b.label(m.id, "ann", "one", None).unwrap();
    let words = Filter {
        text: Some("old words".into()),
        ..Default::default()
    };
    assert!(!b.event_page(0, 100, &words, None).unwrap().1.is_empty());
    assert!(b.event_tail(100, &words).unwrap().1.is_empty());
    let (head, hist) = b.event_tail(100, &Default::default()).unwrap();
    assert!(
        hist.iter()
            .all(|e| e.message.as_ref().is_none_or(|m| m.text == "new words"))
    );
    assert!(!hist.iter().any(|e| e.kind.starts_with("reaction.")));
    assert!(!serde_json::to_string(&hist).unwrap().contains("removed"));
    assert_eq!(b.activity_unread("ann").unwrap(), 2);
    b.mark_activity_seen("ann", head).unwrap();
    b.mark_activity_seen("ann", m.id).unwrap();
    assert_eq!(b.activity_unread("ann").unwrap(), 0);
    assert_eq!(b.activity_seen("ann").unwrap(), head);
    assert_eq!(b.cursor().unwrap(), head);
    assert!(b.mark_activity_seen("ann", head + 1).is_err());
}

#[test]
fn reading_imported_text_and_people_does_not_reveal_connector_receipts() -> Result<()> {
    use board_local::sources::{AuthorInfo, Change, Envelope, Import, Provider};
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("streams.db");
    let mut owner = Board::open(&path)?;
    owner.create_source(
        "owner",
        "mirror",
        Provider::Local,
        "private-remote-name",
        "public",
    )?;
    let envelope = Envelope {
        key: "opaque-delivery-receipt".into(),
        import: Import {
            thread: "private-thread-id".into(),
            external: "sensitive-external-key".into(),
            author: "27".into(),
            profile: Some(AuthorInfo {
                name: "Asha".into(),
                avatar: None,
            }),
            parent: None,
            url: None,
            updated_at: None,
            change: Change::Post {
                text: "message on the board".into(),
                labels: Default::default(),
            },
        },
        raw: serde_json::json!({"secret_fixture":"never-emit"}),
    };
    let m = owner.import_source("owner", "mirror", &envelope)?;
    let local = owner.post("public", "owner", "Local root", &Default::default())?;
    owner.set_outbound("owner", "mirror", true)?;
    owner.start_source_thread("owner", "mirror", local.id)?;
    assert_eq!(
        owner.get(m.id)?.origin.as_ref().unwrap().external,
        "sensitive-external-key"
    );
    owner.set_grant("owner", None, "reader", &Rules::parse("*=read")?)?;
    let token = owner.issue_credential("owner", "reader", None, None)?.token;
    let mut db = Board::open(&path)?;
    db.bind_token(&token)?;
    assert!(db.get(m.id)?.origin.is_none());
    let visible = db.actor_directory(Some("Asha"))?;
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].id, "source:mirror/27");
    assert_eq!(visible[0].kind, "external");
    assert!(!visible[0].verified && !visible[0].local);
    for page in [
        db.events(0, 100)?,
        db.event_tail(100, &Filter::default())?.1,
    ] {
        let serialized = serde_json::to_string(&page)?;
        assert!(serialized.contains("message on the board"));
        for private in [
            "thread.sync_requested",
            "sensitive-external-key",
            "opaque-delivery-receipt",
            "private-thread-id",
            "never-emit",
        ] {
            assert!(
                !serialized.contains(private),
                "{private} escaped event projection"
            );
        }
    }
    owner.with_request(None, "owner", Some("public=all"), |db, _| {
        assert!(db.get(m.id)?.origin.is_none());
        Ok(())
    })?;
    Ok(())
}
