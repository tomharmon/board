use anyhow::Result;
use board_local::{Board, Filter, access::Rules};
use std::collections::BTreeMap;

fn compare(b: &Board) -> Result<()> {
    let mut tally = BTreeMap::<String, (i64, i64, Option<i64>)>::new();
    for c in b.channels()? {
        tally.insert(c.name, (0, 0, None));
    }
    let mut threads: BTreeMap<i64, (i64, Option<i64>)> = BTreeMap::new();
    for m in b.messages(&Default::default(), 500)? {
        let count = tally.get_mut(&m.channel).unwrap();
        count.0 += i64::from(m.re.is_none());
        count.1 += 1;
        count.2 = Some(m.id);
        if let Some(root) = m.re {
            if !m.deleted {
                let t = threads.get_mut(&root).unwrap();
                t.0 += 1;
                t.1 = Some(m.id);
            }
        } else {
            threads.insert(m.id, (0, None));
        }
    }
    let mut expected: Vec<_> = threads
        .into_iter()
        .map(|(id, (n, last))| (id, n, last))
        .collect();
    expected.sort_unstable_by_key(|(id, _, last)| last.unwrap_or(*id));
    let results = b.threads_window(&Filter::default(), None, 500)?;
    assert_eq!(results.snapshot, b.cursor()?);
    assert_eq!(
        results
            .rows
            .iter()
            .map(|t| (t.message.id, t.replies, t.last.as_ref().map(|m| m.id)))
            .collect::<Vec<_>>(),
        expected
    );
    for (name, (roots, total, last)) in tally {
        let c = b.channel(&name)?;
        assert_eq!((c.threads, c.messages, c.last), (roots, total, last));
    }
    Ok(())
}
#[test]
fn v11_upgrade_and_independent_mutations_keep_indexed_read_models_equal_to_the_message_fold()
-> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("projection.db");
    let b = Board::open(&path)?;
    b.create_channel("empty")?;
    let mut roots = Vec::new();
    for n in 0..8 {
        let ch = if n % 2 == 0 { "public" } else { "private" };
        roots.push(
            b.post(ch, "writer", &format!("row {n}"), &Default::default())?
                .id,
        );
    }
    let mut replies = Vec::new();
    for n in 0..32 {
        replies.push(
            b.reply(
                roots[(n * 7) % roots.len()],
                "writer",
                "note",
                &Default::default(),
            )?
            .id,
        );
    }
    for &id in replies.iter().step_by(3) {
        b.delete(id, "writer", None)?;
    }
    b.delete(roots[0], "writer", None)?;
    let head = b.cursor()?;
    drop(b);
    let fixture = rusqlite::Connection::open(&path)?;
    fixture.execute_batch(
        "DROP TRIGGER activity_on_post; DROP TRIGGER activity_on_tombstone;
        DROP TABLE thread_activity; DROP TABLE channel_activity; PRAGMA user_version=11;",
    )?;
    drop(fixture);
    let b = Board::open(&path)?;
    let writer = Board::open(&path)?;
    assert_eq!(b.cursor()?, head, "building read indexes published events");
    compare(&b)?;
    for &id in replies.iter().rev().step_by(4) {
        writer.delete(id, "writer", None)?;
        compare(&b)?;
    }
    let new = writer.reply(roots[2], "writer", "last", &Default::default())?;
    assert_eq!(
        b.threads(
            &Filter {
                channel: Some("public".into()),
                ..Default::default()
            },
            1
        )?[0]
            .last
            .as_ref()
            .unwrap()
            .id,
        new.id
    );
    compare(&b)?;
    b.set_grant("admin", None, "reader", &Rules::parse("public=read")?)?;
    let mut reader = Board::open(&path)?;
    reader.bind_token(&b.issue_credential("admin", "reader", None, None)?.token)?;
    compare(&reader)?;
    let raw = rusqlite::Connection::open(&path)?;
    raw.execute_batch("CREATE TRIGGER reject_post BEFORE UPDATE OF message ON events WHEN new.kind='message.posted' BEGIN SELECT RAISE(ABORT,'test failure'); END;")?;
    let head = b.cursor()?;
    assert!(
        writer
            .reply(roots[2], "writer", "rolled back", &Default::default())
            .is_err()
    );
    assert_eq!(b.cursor()?, head);
    compare(&b)?;
    for (query, index) in [
        (
            "SELECT root,activity,replies FROM thread_activity ORDER BY activity DESC LIMIT 20",
            "thread_activity_all",
        ),
        (
            "SELECT root,activity,replies FROM thread_activity WHERE channel='public' AND activity<150 ORDER BY activity DESC LIMIT 20",
            "thread_activity_channel",
        ),
    ] {
        let plan = raw
            .prepare(&format!("EXPLAIN QUERY PLAN {query}"))?
            .query_map([], |r| r.get::<_, String>(3))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .join("\n");
        assert!(
            plan.contains(index) && !plan.contains("TEMP B-TREE"),
            "{plan}"
        );
    }
    Ok(())
}

#[test]
fn ordinary_feeds_skip_bookkeeping_before_limits_without_losing_the_resume_position() -> Result<()>
{
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("feed.db");
    let b = Board::open(&path)?;
    b.create_channel_as("dev", "owner")?;
    let a = b.post("dev", "owner", "alpha", &Default::default())?;
    b.set_actor_profile("owner", "chief", "Chief")?;
    let z = b.post("dev", "owner", "zeta", &Default::default())?;
    b.react(z.id, "reader", "eyes", false)?;
    b.react(z.id, "reader", "eyes", true)?;
    b.set_grant("owner", None, "reader", &Rules::parse("dev=read")?)?;
    let key = b.issue_credential("owner", "reader", None, None)?.token;
    let head = b.cursor()?;
    let f = Filter::default();
    assert_eq!(b.event_feed(0, 1, &f, None, false)?.1[0].cursor, a.id);
    assert_eq!(b.event_feed(a.id, 1, &f, None, false)?.1[0].cursor, z.id);
    assert_eq!(b.event_feed(head + 999, 1, &f, None, false)?.0, head);
    assert_eq!(
        b.event_feed(0, 1, &f, Some("channel.created"), false)?.1[0].kind,
        "channel.created"
    );
    let (at, ordinary) = b.event_feed(0, 50, &f, None, false)?;
    assert_eq!(at, head);
    assert_eq!(
        ordinary.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        [
            "message.posted",
            "message.posted",
            "reaction.added",
            "reaction.removed"
        ]
    );
    let (at, history) = b.tail_feed(2, &f, false)?;
    assert_eq!(at, head);
    assert_eq!(
        history.iter().map(|e| e.cursor).collect::<Vec<_>>(),
        [a.id, z.id],
        "removed reactions or private bookkeeping used up a history slot"
    );
    assert!(
        b.event_feed(0, 50, &f, None, true)?
            .1
            .iter()
            .any(|e| e.kind == "credential.issued")
    );
    let mut reader = Board::open(path)?;
    reader.bind_token(&key)?;
    for raw in [false, true] {
        assert!(
            !reader
                .event_feed(0, 50, &f, None, raw)?
                .1
                .iter()
                .any(|e| e.kind.starts_with("grant.") || e.kind.starts_with("credential."))
        );
    }
    Ok(())
}

#[test]
fn future_message_and_thread_cursors_resume_at_this_snapshot_not_at_the_requested_number()
-> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("future.db");
    let reader = Board::open(&path)?;
    let writer = Board::open(&path)?;
    writer.post("notes", "author", "first", &Default::default())?;
    writer.create_channel_as("unrelated", "author")?;
    let head = reader.cursor()?;
    let mut f = Filter {
        channel: Some("notes".into()),
        after: Some(i64::MAX),
        ..Default::default()
    };
    let messages = reader.messages_window(&f, None, 1)?;
    let threads = reader.threads_window(&f, None, 1)?;
    assert!(messages.rows.is_empty() && threads.rows.is_empty());
    assert_eq!((messages.cursor, messages.snapshot), (head, head));
    assert_eq!((threads.cursor, threads.snapshot), (head, head));
    let new = writer.post("notes", "author", "next", &Default::default())?;
    f.after = Some(messages.cursor);
    assert_eq!(reader.messages_window(&f, None, 1)?.rows[0].id, new.id);
    f.after = Some(threads.cursor);
    assert_eq!(
        reader.threads_window(&f, None, 1)?.rows[0].message.id,
        new.id
    );
    Ok(())
}

#[test]
fn selected_actor_ids_are_visible_once_or_absent_at_the_returned_snapshot() -> Result<()> {
    let d = tempfile::tempdir()?;
    let path = d.path().join("directory");
    let b = Board::open(&path)?;
    b.set_actor_profile("a", "moss", "Moss")?;
    b.set_actor_profile("b", "mossy", "Mossy")?;
    b.post("public", "a", "visible", &Default::default())?;
    b.post("closed", "b", "not visible", &Default::default())?;
    b.set_grant("owner", None, "reader", &Rules::parse("public=read")?)?;
    let mut reader = Board::open(&path)?;
    reader.bind_token(&b.issue_credential("owner", "reader", None, None)?.token)?;
    let ids = ["b", "missing", "a", "a"].map(str::to_owned);
    let (cursor, entries) = reader.actor_directory_page(Some("moss"), &ids)?;
    assert_eq!(cursor, b.cursor()?);
    assert_eq!(
        entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        ["a"]
    );
    assert_eq!(b.actor_directory_page(Some("moss"), &ids)?.1.len(), 2);
    assert!(
        reader
            .actor_directory_page(None, &vec!["a".into(); 101])
            .is_err()
    );
    let after = cursor;
    let writer = Board::open(&path)?;
    let job = std::thread::spawn(move || -> Result<()> {
        for n in 1..=50 {
            writer.post(
                "public",
                &format!("extra{n}"),
                "visible",
                &Default::default(),
            )?;
        }
        Ok(())
    });
    loop {
        let (cursor, entries) = reader.actor_directory_page(Some("extra"), &[])?;
        assert_eq!(entries.len() as i64, cursor - after);
        if job.is_finished() {
            break;
        }
    }
    job.join().unwrap()?;
    assert_eq!(reader.actor_directory_page(Some("extra"), &[])?.1.len(), 50);
    Ok(())
}

#[test]
fn carried_quote_identity_includes_the_workspace_and_thread_not_just_timestamp() -> Result<()> {
    use board_local::sources::{Change, Envelope, Import, Provider};
    let b = Board::memory()?;
    b.create_source("p", "source", Provider::Slack, "C1", "dev")?;
    let link = "https://example.slack.com/archives/C1/p1234567890000001";
    let imported = b.import_source(
        "p",
        "source",
        &Envelope {
            key: "one".into(),
            raw: serde_json::json!({}),
            import: Import {
                links: vec![],
                external: "1234567890.000001".into(),
                thread: "1234567890.000001".into(),
                author: "writer".into(),
                profile: None,
                parent: None,
                url: Some(link.into()),
                updated_at: None,
                mentioned: Default::default(),
                no_sync: false,
                change: Change::Post {
                    text: "right workspace".into(),
                    labels: Default::default(),
                },
            },
        },
    )?;
    for (url, expected) in [
        (
            format!("{link}?thread_ts=1234567890.000001"),
            Some(imported.id),
        ),
        (format!("{link}?thread_ts=999.000001"), None),
        (link.replace("example.slack.com", "another.slack.com"), None),
    ] {
        let quote = b.post("dev", "p", &url, &Default::default())?.links;
        assert_eq!(quote.len(), 1);
        assert_eq!(quote[0].message, expected);
        assert_eq!(
            quote[0].text.as_deref(),
            expected.map(|_| "right workspace")
        );
    }
    Ok(())
}
