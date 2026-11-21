use anyhow::Result;
use board_local::{Board, ComposerSettings, Draft, PostOptions};

#[test]
fn channel_composer_is_leased_to_its_actor_slot_and_destination_until_an_atomic_send() -> Result<()>
{
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("drafts.db");
    let owner = Board::open(&path)?;
    let grant = owner.set_grant(
        "owner",
        None,
        "agent",
        &board_local::access::Rules::parse("*=read,post")?,
    )?;
    let token = owner.issue_credential("owner", "agent", None, None)?.token;
    let mut first = Board::open(&path)?;
    first.bind_token(&token)?;
    let mut second = Board::open(&path)?;
    second.bind_token(&token)?;
    let make = |channel: &str, body: &str| Draft {
        channel: channel.into(),
        body: body.into(),
        reply_to: None,
    };
    let left = make("work/a", "first");
    let right = make("work/b", "second");
    let opts = ComposerSettings::default();
    let slot = "channel:work/a";
    assert!(
        first
            .save_composer("agent", slot, None, Some((&right, &opts)), None)
            .is_err()
    );
    let stamp = first
        .save_composer("agent", slot, None, Some((&left, &opts)), None)?
        .unwrap();
    assert!(
        owner.load_composer("agent", slot)?.is_none(),
        "OS owner actor and delegated private storage are separate"
    );
    assert!(second.load_composer("owner", slot).is_err());
    second.save_composer("agent", "channel:work/b", None, Some((&right, &opts)), None)?;
    let new = make("work/a", "replacement");
    let rev = second
        .save_composer("agent", slot, Some(&stamp), Some((&new, &opts)), None)?
        .unwrap();
    let head = owner.cursor()?;
    for (ch, scope, s) in [
        ("work/a", slot, &stamp),
        ("work/b", slot, &rev),
        ("work/a", "tasks", &rev),
    ] {
        assert!(
            first
                .post_scoped_composer(ch, "agent", &new.body, &PostOptions::default(), (scope, s))
                .is_err()
        );
    }
    assert_eq!(owner.cursor()?, head);
    let post = first.post_scoped_composer(
        "work/a",
        "agent",
        &new.body,
        &Default::default(),
        (slot, &rev),
    )?;
    assert_eq!(
        (post.channel.as_str(), post.text.as_str()),
        ("work/a", "replacement")
    );
    assert!(second.load_composer("agent", slot)?.is_none());
    assert_eq!(
        second
            .load_composer("agent", "channel:work/b")?
            .unwrap()
            .draft,
        right
    );
    owner.set_grant(
        "owner",
        Some(grant.id),
        "agent",
        &board_local::access::Rules::parse("work/a=read")?,
    )?;
    assert!(second.load_composer("agent", "channel:work/b").is_err());
    Ok(())
}

#[test]
fn saved_newest_guard_cannot_be_removed_rebased_or_moved_outside_its_thread() -> Result<()> {
    let db = Board::open(":memory:")?;
    let root = db.post("work", "reader", "Thread", &Default::default())?.id;
    let seen = db.reply(root, "other", "Seen", &Default::default())?.id;
    let outside = db
        .post("work", "reader", "Different thread", &Default::default())?
        .id;
    let scope = format!("thread:{root}");
    let draft = Draft {
        body: "Local draft".into(),
        channel: "work".into(),
        reply_to: Some(root),
    };
    let settings = ComposerSettings {
        after: Some(seen),
        ..Default::default()
    };
    for bad in [
        ComposerSettings {
            after: Some(outside),
            ..settings.clone()
        },
        ComposerSettings {
            later: true,
            ..settings.clone()
        },
    ] {
        assert!(
            db.save_composer("reader", &scope, None, Some((&draft, &bad)), None)
                .is_err()
        );
    }
    let stamp = db
        .save_composer("reader", &scope, None, Some((&draft, &settings)), None)?
        .unwrap();
    let newest = db.reply(root, "other", "Unseen", &Default::default())?.id;
    for after in [None, Some(newest), Some(seen)] {
        let error = db
            .reply_composer(
                root,
                "reader",
                &draft.body,
                &PostOptions {
                    after,
                    ..Default::default()
                },
                (&scope, &stamp),
            )
            .unwrap_err();
        assert!(error.to_string().contains(if after == Some(seen) {
            "latest message"
        } else {
            "Draft changed"
        }));
    }
    assert_eq!(db.thread(root)?.len(), 3);
    assert_eq!(db.load_composer("reader", &scope)?.unwrap().stamp, stamp);
    let settings = ComposerSettings {
        after: Some(newest),
        ..settings
    };
    let stamp = db
        .save_composer(
            "reader",
            &scope,
            Some(&stamp),
            Some((&draft, &settings)),
            None,
        )?
        .unwrap();
    let posted = db.reply_composer(
        root,
        "reader",
        &draft.body,
        &PostOptions {
            after: Some(newest),
            ..Default::default()
        },
        (&scope, &stamp),
    )?;
    assert_eq!((posted.re, posted.reply_to), (Some(root), None));
    assert!(db.load_composer("reader", &scope)?.is_none());
    Ok(())
}

#[test]
fn genuine_v7_and_v8_databases_upgrade_and_retire_a_recovered_draft_atomically() -> Result<()> {
    for (version, fixture, channel) in [
        (
            7,
            &include_bytes!("fixtures/draft-v7.sqlite3.gz")[..],
            "ops",
        ),
        (
            8,
            &include_bytes!("fixtures/draft-v8.sqlite3.gz")[..],
            "studio/review",
        ),
    ] {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("history.sqlite3");
        std::io::copy(
            &mut flate2::read::GzDecoder::new(fixture),
            &mut std::fs::File::create(&file)?,
        )?;
        let before = rusqlite::Connection::open(&file)?;
        assert_eq!(
            before.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?,
            version
        );
        assert!(!before.table_exists(None, "composer_files")?);
        drop(before);
        let db = Board::open(&file)?;
        let old = db.load_composer("reader", "thread:1")?.unwrap();
        assert_eq!(old.settings, ComposerSettings::default());
        assert_eq!(
            old.draft,
            Draft {
                channel: channel.into(),
                body: "A real old draft".into(),
                reply_to: Some(1)
            }
        );
        assert!(old.files.is_empty());
        let settings = ComposerSettings {
            no_sync: true,
            ..Default::default()
        };
        let stamp = db
            .save_composer(
                "reader",
                "thread:1",
                Some(&old.stamp),
                Some((&old.draft, &settings)),
                Some(&[("note.txt".into(), b"old".to_vec())]),
            )?
            .unwrap();
        drop(db);
        let db = Board::open(&file)?;
        let recovered = db.load_composer("reader", "thread:1")?.unwrap();
        assert_eq!(recovered.settings, settings);
        assert_eq!(recovered.files[0].1, b"old");
        let opts = PostOptions {
            no_sync: true,
            files: recovered.files,
            ..Default::default()
        };
        assert!(
            db.reply_composer(
                1,
                "reader",
                &old.draft.body,
                &PostOptions {
                    no_sync: false,
                    ..opts.clone()
                },
                ("thread:1", &stamp)
            )
            .is_err()
        );
        assert!(
            db.reply_composer(
                1,
                "reader",
                &old.draft.body,
                &PostOptions {
                    files: vec![("note.txt".into(), b"bad".to_vec())],
                    ..opts.clone()
                },
                ("thread:1", &stamp)
            )
            .is_err()
        );
        assert!(
            db.reply_composer(
                1,
                "reader",
                &old.draft.body,
                &opts,
                ("thread:1", &old.stamp)
            )
            .is_err()
        );
        assert!(
            db.reply_composer(1, "reader", "invalid\0", &opts, ("thread:1", &stamp))
                .is_err()
        );
        assert_eq!(
            db.load_composer("reader", "thread:1")?.unwrap().stamp,
            stamp
        );
        let m = db.reply_composer(1, "reader", &old.draft.body, &opts, ("thread:1", &stamp))?;
        assert!(m.no_sync);
        assert_eq!(db.download(m.attachments[0].id)?.1, b"old");
        assert!(db.load_composer("reader", "thread:1")?.is_none());
        assert!(
            db.reply_composer(1, "reader", &old.draft.body, &opts, ("thread:1", &stamp))
                .is_err()
        );
        assert_eq!(db.thread(1)?.len(), 2);
    }
    Ok(())
}
