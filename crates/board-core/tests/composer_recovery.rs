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
    let outside_thread = ComposerSettings {
        after: Some(outside),
        ..settings.clone()
    };
    assert!(
        db.save_composer(
            "reader",
            &scope,
            None,
            Some((&draft, &outside_thread)),
            None
        )
        .is_err()
    );
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
