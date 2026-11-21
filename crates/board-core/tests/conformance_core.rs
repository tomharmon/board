use anyhow::Result;
use board_local::{Board, Filter, PostOptions, access::Rules, errors::classify};
use serde_json::json;
use std::collections::BTreeMap;

#[test]
fn mentions_resolve_only_visible_full_handles_prefer_thread_authors_and_preserve_retry_truth()
-> Result<()> {
    let d = tempfile::tempdir()?;
    let path = d.path().join("mentions.db");
    let b = Board::open(&path)?;
    for (id, name) in [("a", "Ava"), ("b", "Bo")] {
        b.set_actor_profile(id, "build", name)?;
    }
    b.set_actor_profile("ann/bot", "ops/helper", "Helper")?;
    let ambiguous = b.post(
        "public",
        "quinn",
        "Ask @build, @Build, @helper and @ghost. Code: `@build`",
        &Default::default(),
    )?;
    assert!(ambiguous.text.contains("Ask @build, @Build"));
    assert_eq!(ambiguous.mentions.len(), 3);
    assert!(ambiguous.mentions[0].actor.is_none());
    assert_eq!(ambiguous.mentions[0].candidates.len(), 2);
    assert_eq!(ambiguous.mentions[1].candidates[0].id, "ann/bot");
    assert!(ambiguous.mentions[2].candidates.is_empty());
    let root = b.post("public", "b", "writing in this thread", &Default::default())?;
    let picked = b.reply(
        root.id,
        "quinn",
        "@BUILD can you help?",
        &Default::default(),
    )?;
    assert_eq!(picked.text, "@[b] can you help?");
    assert_eq!(picked.mentions[0].actor.as_ref().unwrap().id, "b");
    assert!(
        b.get(picked.id)?.mentions.is_empty(),
        "private suggestions aren't persisted into read/event projections"
    );
    b.set_grant(
        "admin",
        None,
        "viewer",
        &Rules::parse("restricted=read,post")?,
    )?;
    let mut scoped = Board::open(&path)?;
    scoped.bind_token(&b.issue_credential("admin", "viewer", None, None)?.token)?;
    let unknown = scoped.post("restricted", "viewer", "@build", &Default::default())?;
    assert_eq!(unknown.text, "@build");
    assert!(unknown.mentions[0].candidates.is_empty());
    let once = PostOptions {
        key: Some("lost-send".into()),
        ..Default::default()
    };
    let first = b.post("public", "quinn", "@future", &once)?;
    b.set_actor_profile("p", "future", "Future")?;
    let retry = b.post("public", "quinn", "@future", &once)?;
    assert_eq!((retry.id, retry.text.as_str()), (first.id, "@future"));
    assert!(retry.mentions[0].actor.is_none());
    assert_eq!(retry.mentions[0].candidates[0].id, "p");
    let long = format!("{} @future", "a".repeat(9992));
    b.set_actor_profile("quite-a-long-name", "long", "Long")?;
    let long = long.replace(" @future", "aa @long");
    assert_eq!(long.len(), 10_000);
    let limited = b.post("public", "quinn", &long, &Default::default())?;
    assert_eq!(limited.text, long);
    assert!(limited.mentions[0].actor.is_none());
    Ok(())
}

#[test]
fn profile_headers_edit_timestamps_limits_and_noop_events_hold_in_the_database() -> Result<()> {
    let mut b = Board::memory()?;
    assert_eq!(
        board_local::local::profile_name("FIRST-Agent")?,
        "first-agent"
    );
    for invalid in [
        "1starts-with-digit",
        "under_score",
        "has.dot",
        &"a".repeat(33),
    ] {
        assert!(
            b.with_profile_request(None, "owner", None, Some(invalid), |_, _| Ok(()))
                .is_err()
        );
        assert!(board_local::local::profile_name(invalid).is_err());
    }
    b.with_profile_request(
        None,
        "owner",
        None,
        Some("Local; avatar=\"http://127.0.0.1/picture\""),
        |_, who| {
            assert_eq!(who, "owner/local");
            Ok(())
        },
    )?;
    let root = b.with_profile_request(
        None,
        "owner",
        None,
        Some(r#" "Helper";flag;v=1.02;old=:QQ==:;note="not;an\"avatar";avatar="https://old.invalid";avatar="https://example.invalid/a.png" "#),
        |b, actor| {
            assert_eq!(actor, "owner/helper");
            b.post("public", actor, "written locally", &Default::default())
        },
    )?;
    let profile = b.get(root.id)?.author.unwrap();
    assert_eq!(
        (
            profile.kind.as_str(),
            profile.name.as_str(),
            profile.via.as_deref()
        ),
        ("agent", "helper", Some("owner"))
    );
    assert_eq!(
        profile.avatar.as_deref(),
        Some("https://example.invalid/a.png")
    );
    for malformed in [
        "helper;avatar=bare",
        "helper;avatar",
        "helper ;flag",
        "helper;p=?3",
        "helper;Flag",
        "helper;p=1234567890123456",
        r#"helper;bad="unfinished"#,
        r#"helper;bad="bad\q""#,
    ] {
        assert!(
            b.with_profile_request(None, "owner", None, Some(malformed), |_, _| Ok(()))
                .is_err(),
            "{malformed}"
        );
    }
    b.with_profile_request(None, "owner", None, Some("other"), |b, actor| {
        assert!(b.edit(root.id, actor, "not mine", None).is_err());
        Ok(())
    })?;
    assert!(root.edited_at.is_none());
    let edit = b.edit(
        root.id,
        "owner",
        "principal corrects it",
        Some(root.revision),
    )?;
    assert!(edit.edited_at.is_some());
    assert_eq!(edit.actor, root.actor);
    b.react(root.id, "owner", ":EYES:", false)?;
    let head = b.cursor()?;
    assert_eq!(b.react(root.id, "owner", "👀", false)?.version, head);
    assert!(b.react(root.id, "owner", "bad:name", false).is_err());
    assert!(b.react(root.id, "owner", "constructor", false).is_err());
    assert!(
        b.labels(
            root.id,
            "owner",
            &Default::default(),
            &[],
            &(0..17).map(|i| (format!("k{i}"), None)).collect()
        )
        .is_err()
    );
    assert!(
        b.labels(
            root.id,
            "owner",
            &Default::default(),
            &[],
            &[("bad key".into(), None)].into()
        )
        .is_err()
    );
    b.labels(
        root.id,
        "owner",
        &Default::default(),
        &[],
        &[("board.task".into(), None)].into(),
    )?;
    assert_eq!(b.cursor()?, head);
    assert!(
        b.post("public", "owner", &"🌟".repeat(5001), &Default::default())
            .is_err()
    );
    assert!(
        b.label(root.id, "owner", "k", Some(&"🌟".repeat(129)))
            .is_err()
    );
    assert!(
        b.post("public", "owner", &"🌟".repeat(5000), &Default::default())
            .is_ok()
    );
    let filter = Filter {
        text: Some("x".repeat(201)),
        ..Default::default()
    };
    assert!(b.threads(&filter, 1).is_err());
    assert!(b.event_page(0, 1, &filter, None).is_err());
    assert!(
        b.actor_directory(None)?
            .iter()
            .any(|a| a.avatar == profile.avatar)
    );
    Ok(())
}

#[test]
fn source_filter_selects_inbound_origin_not_outbound_locations_before_paging() -> Result<()> {
    use board_local::sources::{AuthorInfo, Change, Envelope, Import, Provider};
    let b = Board::memory()?;
    b.create_source("owner", "origin", Provider::Local, "remote-room", "work")?;
    let inbound = b.import_source(
        "owner",
        "origin",
        &Envelope {
            key: "in1".into(),
            raw: json!({}),
            import: Import {
                links: Vec::new(),
                external: "msg".into(),
                thread: "thread".into(),
                author: "U123".into(),
                profile: Some(AuthorInfo {
                    name: "Jürgen Ubl".into(),
                    avatar: None,
                }),
                parent: None,
                url: None,
                updated_at: None,
                mentioned: BTreeMap::new(),
                no_sync: false,
                change: Change::Post {
                    text: "inbound".into(),
                    labels: BTreeMap::new(),
                },
            },
        },
    )?;
    let out = b.post("work", "owner", "native", &Default::default())?;
    for (from, id) in [("board", out.id), ("origin", inbound.id)] {
        let f = Filter {
            from: Some(from.into()),
            ..Default::default()
        };
        assert_eq!(b.messages(&f, 1)?[0].id, id);
        assert_eq!(b.threads(&f, 1)?[0].message.id, id);
        assert_eq!(
            b.event_page(0, 1, &f, Some("message.posted"))?.1[0]
                .message
                .as_ref()
                .unwrap()
                .id,
            id
        );
    }
    let (_, directory) =
        b.actor_directory_page(Some("jurgen"), std::slice::from_ref(&inbound.actor))?;
    assert_eq!(directory.len(), 1);
    assert_eq!(directory[0].handle, "origin/jurgen-ubl");
    let ping = b.post("work", "owner", "@origin/jurgen-ubl", &Default::default())?;
    assert_eq!(ping.text, format!("@[{}]", inbound.actor));
    for (by, mentions, want) in [
        (Some("origin/jurgen-ubl"), None, inbound.id),
        (None, Some("origin/jurgen-ubl"), ping.id),
    ] {
        let f = Filter {
            by: by.map(str::to_owned),
            mentions: mentions.map(str::to_owned),
            ..Default::default()
        };
        assert_eq!(b.messages(&f, 1)?[0].id, want);
    }
    let e = b
        .post(
            "work",
            "owner",
            "```json#missing@1\n{}\n```",
            &Default::default(),
        )
        .unwrap_err();
    assert_eq!(classify(&e).1, "schema_not_found");
    assert_eq!(
        classify(&b.schema("BAD", 1).unwrap_err()).1,
        "invalid_schema_id"
    );
    Ok(())
}

#[test]
fn channel_prefix_pages_are_not_subtrees_and_search_treats_punctuation_literally() -> Result<()> {
    let d = tempfile::tempdir()?;
    let path = d.path().join("read.db");
    let b = Board::open(&path)?;
    for channel in ["dev", "dev/closed", "dev/log", "developer", "misc"] {
        b.post(channel, "a", channel, &Default::default())?;
    }
    let m = b.post("dev/log", "a", "Crème_15% complete", &Default::default())?;
    b.reply(m.id, "a", "CRÈME stray", &Default::default())?;
    b.set_grant(
        "os",
        None,
        "reader",
        &Rules::parse("*=read;dev/closed=none")?,
    )?;
    let mut reader = Board::open(&path)?;
    reader.bind_token(&b.issue_credential("os", "reader", None, None)?.token)?;
    let (head, page, next) = reader.channels_window("", None, Some("dev"), 1)?;
    assert_eq!(head, b.cursor()?);
    assert_eq!(page[0].name, "dev");
    assert_eq!(next.as_deref(), Some("dev"));
    let (_, page, next) = reader.channels_window(&next.unwrap(), None, Some("dev"), 2)?;
    assert_eq!(
        page.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["dev/log", "developer"]
    );
    assert!(next.is_none());
    let (_, desc, _) = reader.channels_window("", None, Some("dev/"), 20)?;
    assert_eq!(desc.len(), 1);
    let (_, scope, _) = reader.channels_window("", Some("dev/"), None, 20)?;
    assert_eq!(scope.len(), 2);
    for (text, n) in [
        ("CrèMe", 2),
        ("_", 1),
        ("%", 1),
        ("_15%", 1),
        ("_STRAY%", 0),
    ] {
        assert_eq!(
            reader
                .messages(
                    &Filter {
                        text: Some(text.into()),
                        channel: Some("dev/".into()),
                        ..Default::default()
                    },
                    20
                )?
                .len(),
            n
        );
    }
    b.edit(m.id, "a", "moved", None)?;
    let f = Filter {
        text: Some("crème".into()),
        ..Default::default()
    };
    assert_eq!(reader.threads(&f, 1)?[0].message.id, m.id);
    assert_eq!(reader.messages(&f, 10)?.len(), 1);
    Ok(())
}

#[test]
fn schema_value_formats_and_numeric_steps_are_validated_inside_nested_alternatives() -> Result<()> {
    let b = Board::memory()?;
    for (i, (field, good, bad)) in [
        (
            json!({"format":"time"}),
            json!("23:59:60-99"),
            json!("12:05:60Z"),
        ),
        (
            json!({"format":"date-time"}),
            json!("2000-02-29 12:34:56"),
            json!("1900-02-29 12:34:56"),
        ),
        (
            json!({"format":"email"}),
            json!("local+tag@example.net"),
            json!("a@localhost"),
        ),
        (
            json!({"format":"hostname"}),
            json!("example.net."),
            json!("not..valid"),
        ),
        (
            json!({"format":"ipv4"}),
            json!("001.002.003.004"),
            json!("001.002.003.256"),
        ),
        (
            json!({"format":"uri"}),
            json!("http://localhost"),
            json!("http:"),
        ),
        (
            json!({"format":"uuid"}),
            json!("URN:UUID:aaaa0001-0000-0000-0000-000000000001"),
            json!("aaaaaaaa"),
        ),
        (json!({"multipleOf":0.1}), json!(2.0), json!(-0.3)),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("field{i}");
        let def = json!({"type":"object","properties":{"x":{"anyOf":[false,field]}}});
        b.register_schema_as(&name, 1, "", &def, "owner")?;
        let body = |x| format!("```json#{name}@1\n{}\n```", json!({"x":x}));
        b.post("test", "owner", &body(good), &Default::default())?;
        let head = b.cursor()?;
        assert_eq!(
            classify(
                &b.post("test", "owner", &body(bad), &Default::default())
                    .unwrap_err()
            )
            .1,
            "invalid_block"
        );
        assert_eq!(b.cursor()?, head);
    }
    Ok(())
}

#[test]
fn self_selectors_include_only_the_callers_own_id_and_handle_not_child_profiles() -> Result<()> {
    let d = tempfile::tempdir()?;
    let path = d.path().join("me.db");
    let b = Board::open(&path)?;
    let raw = b.post(
        "public",
        "sender",
        "@dispatch before it was registered",
        &Default::default(),
    )?;
    b.set_actor_profile("oncall", "dispatch", "On call")?;
    b.set_grant("admin", None, "oncall", &Rules::parse("public=read")?)?;
    let me = b.post("public", "oncall", "@dispatch", &Default::default())?;
    let bot = b.post("public", "oncall/bot", "@[oncall/bot]", &Default::default())?;
    let qualified = b.post(
        "public",
        "sender",
        "@[directory:Example+profile] and @[source:local/Person]",
        &Default::default(),
    )?;
    let mut reader = Board::open(&path)?;
    reader.bind_token(&b.issue_credential("admin", "oncall", None, None)?.token)?;
    for (by, mentions, want) in [
        (Some("me"), None, vec![me.id]),
        (Some("oncall"), None, vec![me.id, bot.id]),
        (Some("DISPATCH"), None, vec![me.id, bot.id]),
        (None, Some("me"), vec![raw.id, me.id]),
        (None, Some("oncall"), vec![me.id, bot.id]),
        (None, Some("directory:Example"), vec![qualified.id]),
        (None, Some("source:local"), vec![qualified.id]),
    ] {
        let mut f = Filter {
            by: by.map(str::to_owned),
            mentions: mentions.map(str::to_owned),
            ..Default::default()
        };
        f.bind_actor("oncall");
        assert_eq!(
            reader
                .messages(&f, 100)?
                .iter()
                .map(|m| m.id)
                .collect::<Vec<_>>(),
            want
        );
        assert_eq!(
            reader
                .event_page(0, 100, &f, Some("message.posted"))?
                .1
                .iter()
                .map(|e| e.message.as_ref().unwrap().id)
                .collect::<Vec<_>>(),
            want
        );
        assert_eq!(
            reader
                .event_tail(100, &f)?
                .1
                .iter()
                .filter(|e| e.kind == "message.posted")
                .map(|e| e.message.as_ref().unwrap().id)
                .collect::<Vec<_>>(),
            want
        );
    }
    Ok(())
}
