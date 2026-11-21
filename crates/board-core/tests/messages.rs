use anyhow::Result;
use board_local::{Board, Filter, PostOptions};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::{Arc, Barrier};
use std::thread;

#[test]
fn message_workflow_profiles_attachments_and_tombstones() -> Result<()> {
    let db = Board::memory()?;
    let opts = PostOptions {
        labels: BTreeMap::from([("status".into(), "open".into())]),
        files: vec![("evidence.bin".into(), vec![0, 10, 255])],
        ..Default::default()
    };
    let root = db.post("build/ci", "ada/tester", "Follow up", &opts)?;
    let reply = db.reply(root.id, "bob", "checked", &Default::default())?;
    let nested = db.reply(reply.id, "ada", "thanks", &Default::default())?;
    assert_eq!(nested.re, Some(root.id));
    assert_eq!(nested.reply_to, Some(reply.id));
    assert_eq!(db.thread(nested.id)?, [root.clone(), reply, nested]);
    let channel = &db.channels()?[0];
    assert_eq!(
        (channel.name.as_str(), channel.threads, channel.messages),
        ("build/ci", 1, 3)
    );
    assert_eq!(db.download(root.attachments[0].id)?.1, [0, 10, 255]);
    assert!(db.edit(root.id, "ada/sibling", "not mine", None).is_err());
    assert!(db.delete(root.id, "bob", None).is_err());
    let reacted = db.react(root.id, "bob", ":eyes:", false)?;
    let labeled = db.label(root.id, "ada", "status", Some("closed"))?;
    assert_eq!(reacted.revision, root.revision);
    assert_eq!(labeled.revision, root.revision);
    assert!(root.version < reacted.version && reacted.version < labeled.version);
    assert_eq!(
        db.react(root.id, "bob", "eyes", false)?.version,
        labeled.version
    );
    let edited = db.edit(root.id, "ada", "corrected", Some(root.revision))?;
    assert!(edited.revision > root.revision);
    assert!(db.delete(root.id, "ada", Some(root.revision)).is_err());
    let removed = db.delete(root.id, "ada", Some(edited.revision))?;
    assert!(removed.deleted && removed.revision > edited.revision);
    assert!(
        removed.text.is_empty()
            && removed.attachments.is_empty()
            && removed.labels.is_empty()
            && removed.reactions.is_empty()
    );
    assert_eq!(db.thread(root.id)?.len(), 3);
    assert_eq!(
        db.download(root.attachments[0].id)?.1,
        [0, 10, 255],
        "a tombstone drops its placement, not the immutable upload"
    );
    assert!(db.edit(root.id, "ada", "revive", None).is_err());
    assert!(db.label(root.id, "bob", "status", Some("open")).is_err());
    assert!(db.react(root.id, "bob", "eyes", false).is_err());
    Ok(())
}

#[test]
fn local_blocks_are_immutable_and_validate_posts_and_edits() -> Result<()> {
    let db = Board::memory()?;
    let schema = json!({"type":"object","required":["value"],"properties":{"value":{"type":"integer","minimum":1}},"additionalProperties":false});
    assert!(db.register_schema_as("count", 1, "Count", &schema, "ada")?);
    let head = db.cursor()?;
    assert!(!db.register_schema_as("count", 1, "Count", &schema, "bob")?);
    assert!(db.register_schema("count", 1, "Changed", &schema).is_err());
    assert!(
        db.register_schema(
            "remote",
            1,
            "",
            &json!({"type":"object","$ref":"another#entry"})
        )
        .is_err()
    );
    assert!(db.register_schema("git-commit", 2, "", &schema).is_err());
    let valid = "Changes:\n```json#count@1\n{\"value\":2}\n```";
    let invalid = valid.replace('2', "0");
    assert!(db.post("ci", "ada", &invalid, &Default::default()).is_err());
    assert!(
        db.post(
            "ci",
            "ada",
            &valid.replace("count@1", "absent@2"),
            &Default::default()
        )
        .is_err()
    );
    assert_eq!(db.cursor()?, head);
    let msg = db.post("ci", "ada", valid, &Default::default())?;
    assert_eq!(msg.blocks.len(), 1);
    assert_eq!(msg.blocks[0].data, json!({"value":2}));
    assert!(db.edit(msg.id, "ada", &invalid, None).is_err());
    assert_eq!(msg, db.get(msg.id)?);
    let quoted = format!("~~~~\n{invalid}\n~~~~");
    assert!(
        db.post("ci", "ada", &quoted, &Default::default())?
            .blocks
            .is_empty()
    );
    assert_eq!(db.events(0, 100)?[0].kind, "schema.registered");
    Ok(())
}

#[test]
fn local_deployment_schema_validates_without_opening_its_address() -> Result<()> {
    let b = Board::memory()?;
    b.register_schema_as("aaa", 2, "", &json!({"type":"object"}), "editor")?;
    let schemas = b.schemas()?;
    assert_eq!(
        schemas.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        ["git-commit@1", "deployment@1", "aaa@2"]
    );
    assert!(schemas[1].built_in && schemas[1].by.is_none() && schemas[1].at.is_none());
    assert!(
        !schemas[2].built_in
            && schemas[2].by.as_deref() == Some("editor")
            && schemas[2].at.is_some()
    );
    assert!(
        b.register_schema("deployment", 2, "", &json!({"type":"object"}))
            .is_err()
    );
    let block = |data: serde_json::Value| format!("```json#deployment@1\n{data}\n```");
    let fresh = b.post(
        "dev",
        "ada",
        &block(json!({"url":"http://127.0.0.1:4711/status","gitSha":"a".repeat(64)})),
        &Default::default(),
    )?;
    assert_eq!(fresh.blocks[0].schema, "deployment");
    let before = b.cursor()?;
    for invalid in [
        json!({}),
        json!({"url":"file:///tmp/read-me"}),
        json!({"url":"https:///no-host"}),
        json!({"url":"https://localhost","gitSha":"123abc"}),
        json!({"url":"https://localhost","unknown":true}),
    ] {
        assert!(
            b.post("dev", "ada", &block(invalid), &Default::default())
                .is_err()
        );
    }
    assert!(
        b.edit(fresh.id, "ada", &fresh.text.replace("@1", "@01"), None)
            .is_err()
    );
    assert_eq!(b.cursor()?, before);
    Ok(())
}

#[test]
fn a_v1_deployment_schema_keeps_its_meaning_and_registrant_after_upgrade() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("schemas.db");
    let schema = json!({"type":"object","required":["run"]});
    let db = Board::open(&path)?;
    db.register_schema_as("old-name", 7, "legacy", &schema, "old-editor")?;
    drop(db);
    let fixture = rusqlite::Connection::open(&path)?;
    fixture.execute_batch("DROP TABLE schema_origins; DELETE FROM schemas WHERE name='deployment';
        UPDATE schemas SET name='deployment' WHERE name='old-name';
        UPDATE events SET payload=json_set(payload,'$.name','deployment') WHERE kind='schema.registered';
        PRAGMA user_version=1;")?;
    drop(fixture);
    let db = Board::open(&path)?;
    let rows = db.schemas()?;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].id, "deployment@7");
    assert_eq!(rows[1].schema, schema);
    assert!(!rows[1].built_in && rows[1].by.as_deref() == Some("old-editor"));
    assert!(!db.register_schema_as("deployment", 7, "legacy", &schema, "other")?);
    assert!(db.register_schema_as("deployment", 8, "legacy", &schema, "old-editor")?);
    Ok(())
}

#[test]
fn idempotency_and_latest_guards_survive_changes_and_restart() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.db");
    let db = Board::open(&path)?;
    let root = db.post("dev", "ada", "root", &Default::default())?;
    let mut opts = PostOptions {
        key: Some("job-1".into()),
        after: Some(root.id),
        files: vec![("result".into(), vec![4])],
        ..Default::default()
    };
    let (first, created) = db.reply_with_outcome(root.id, "ada/test", "once", &opts)?;
    assert!(created);
    let last = db.reply(first.id, "bob", "later", &Default::default())?;
    db.delete(last.id, "bob", None)?;
    let head = db.cursor()?;
    drop(db);
    let db = Board::open(path)?;
    opts.after = Some(i64::MAX);
    assert_eq!(
        db.reply_with_outcome(root.id, "ada/test", "once", &opts)?,
        (first.clone(), false)
    );
    opts.files[0].1.push(1);
    opts.labels.insert("status".into(), "different".into());
    assert_eq!(
        db.reply(root.id, "ada/test", "different words", &opts)?,
        first
    );
    assert_eq!(db.posted("ada/test", "job-1")?, first);
    assert!(db.posted("ada", "job-1").is_err());
    assert!(db.reply(root.id, "ada/other", "once", &opts).is_err());
    assert!(
        db.reply(first.id, "ada/test", "different reply target", &opts)
            .is_err()
    );
    assert_eq!(db.cursor()?, head);
    opts.key = None;
    assert!(
        db.reply(root.id, "ada/test", "wrong thread or missing", &opts)
            .is_err()
    );
    opts.after = Some(first.id);
    assert!(
        db.reply(root.id, "ada/test", "deleted still counts", &opts)
            .is_err()
    );
    opts.after = Some(last.id);
    assert!(db.reply(root.id, "ada/test", "fresh", &opts).is_ok());
    Ok(())
}

#[test]
fn a_key_binds_to_the_exact_actor_and_placement_not_a_request_body() -> Result<()> {
    use board_local::errors::{Kind, classify};
    let b = Board::memory()?;
    let key = PostOptions {
        key: Some("root".into()),
        ..Default::default()
    };
    let root = b.post("dev", "ada/agent", "initial", &key)?;
    let edited = b.edit(root.id, "ada", "corrected", None)?;
    let head = b.cursor()?;
    assert_eq!(
        b.post_with_outcome("dev", "ada/agent", "rewritten", &key)?,
        (edited.clone(), false)
    );
    for different in [
        b.post("elsewhere", "ada/agent", "initial", &key),
        b.post("dev", "ada", "initial", &key),
        b.reply(root.id, "ada/agent", "initial", &key),
    ] {
        assert_eq!(
            classify(&different.unwrap_err()),
            (Kind::Conflict, "duplicate_id")
        );
    }
    for who in ["ada", "ada/other", "bob"] {
        assert_eq!(
            classify(&b.posted(who, "root").unwrap_err()),
            (Kind::Missing, "post_not_found")
        );
    }
    assert_eq!(b.posted("ada/agent", "root")?, edited);
    assert_eq!(b.cursor()?, head);
    let child = b.reply(root.id, "bob", "a child", &Default::default())?;
    let nested_key = PostOptions {
        key: Some("nested".into()),
        ..Default::default()
    };
    let nested = b.reply(child.id, "ada", "nested", &nested_key)?;
    assert_eq!(nested.reply_to, Some(child.id));
    assert_eq!(b.reply(child.id, "ada", "changed", &nested_key)?, nested);
    assert_eq!(
        classify(&b.reply(root.id, "ada", "changed", &nested_key).unwrap_err()).1,
        "duplicate_id"
    );
    let deleted = b.delete(root.id, "ada", None)?;
    assert_eq!(b.post("dev", "ada/agent", "not a revival", &key)?, deleted);
    Ok(())
}

fn race<F, T>(db: &std::path::Path, action: F) -> Result<Vec<Result<T>>>
where
    F: Fn(Board, usize) -> Result<T> + Sync,
    T: Send,
{
    // Separate connections, synchronized just before the conditional write, like local clients.
    let clients = (0..6)
        .map(|_| Board::open(db))
        .collect::<Result<Vec<_>>>()?;
    let barrier = Barrier::new(clients.len());
    Ok(thread::scope(|s| {
        clients
            .into_iter()
            .enumerate()
            .map(|(i, board)| {
                let (gate, op) = (&barrier, &action);
                s.spawn(move || {
                    gate.wait();
                    op(board, i)
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect()
    }))
}

#[test]
fn independent_writers_linearize_retry_revision_labels_and_latest_reply() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("race.db");
    let db = Board::open(&path)?;
    let opts = PostOptions {
        key: Some("op".into()),
        ..Default::default()
    };
    let tries = race(&path, |b, _| {
        b.post_with_outcome("dev", "ada", "original", &opts)
    })?
    .into_iter()
    .collect::<Result<Vec<_>>>()?;
    let id = tries[0].0.id;
    assert!(tries.iter().all(|(msg, _)| msg.id == id));
    assert_eq!(tries.iter().filter(|(_, made)| *made).count(), 1);
    let wins = race(&path, |b, i| {
        b.edit(id, "ada", &format!("edit {i}"), Some(id))
    })?;
    assert_eq!(wins.iter().filter(|r| r.is_ok()).count(), 1);
    let cond = BTreeMap::from([("owner".into(), None)]);
    let wins = race(&path, |b, i| {
        b.labels(
            id,
            "ada",
            &BTreeMap::from([("owner".into(), format!("p{i}"))]),
            &[],
            &cond,
        )
    })?;
    assert_eq!(wins.iter().filter(|r| r.is_ok()).count(), 1);
    let opts = PostOptions {
        after: Some(id),
        ..Default::default()
    };
    let wins = race(&path, |b, i| {
        b.reply(id, "ada", &format!("reply {i}"), &opts)
    })?;
    assert_eq!(wins.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(db.events(0, 100)?.len(), 4);
    let competing = PostOptions {
        key: Some("board-wide".into()),
        ..Default::default()
    };
    let wins = race(&path, |b, i| {
        b.post("dev", &format!("actor{i}"), "one owner", &competing)
    })?;
    assert_eq!(wins.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(db.events(0, 100)?.len(), 5);
    Ok(())
}

#[test]
fn read_snapshot_matches_cursor_during_concurrent_posts() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("snap.db");
    let reader = Board::open(&db_path)?;
    let writer = Board::open(&db_path)?;
    let root = reader
        .post("live", "reader", "start", &Default::default())?
        .id;
    let gate = Arc::new(Barrier::new(2));
    let start = gate.clone();
    let worker = thread::spawn(move || -> Result<()> {
        start.wait();
        for _ in 0..60 {
            writer.reply(root, "writer", "one committed event", &Default::default())?;
        }
        Ok(())
    });
    gate.wait();
    loop {
        let (cursor, thread) = reader.thread_page(root, &Filter::default())?;
        assert_eq!(cursor, thread.last().unwrap().id);
        assert_eq!(thread.len() as i64, cursor);
        if worker.is_finished() {
            break;
        }
    }
    worker.join().unwrap()?;
    assert_eq!(reader.thread(root)?.len(), 61);
    Ok(())
}

#[test]
fn fault_during_event_snapshot_rolls_back_post_and_attachments() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("fault.db");
    let db = Board::open(&path)?;
    let inject = rusqlite::Connection::open(path)?;
    inject.execute_batch("CREATE TRIGGER fail_snapshot BEFORE UPDATE ON events BEGIN SELECT raise(ABORT,'simulated failure'); END;")?;
    let opts = PostOptions {
        files: vec![("local".into(), vec![42])],
        key: Some("retry".into()),
        ..Default::default()
    };
    assert!(db.post("only-on-success", "ada", "", &opts).is_err());
    assert!(db.channels()?.is_empty());
    assert_eq!(db.cursor()?, 0);
    inject.execute_batch("DROP TRIGGER fail_snapshot")?;
    let msg = db.post("only-on-success", "ada", "", &opts)?;
    assert_eq!(msg.attachments.len(), 1);
    assert_eq!(db.events(0, 10)?[0].message.as_ref(), Some(&msg));
    Ok(())
}

#[test]
fn subtree_search_event_snapshots_and_filtered_page_resume() -> Result<()> {
    let db = Board::memory()?;
    let root = db.post(
        "inbox/a",
        "ada/agent",
        "Caf\u{e9} @bo/helper",
        &PostOptions {
            labels: BTreeMap::from([("stage".into(), "open".into())]),
            ..Default::default()
        },
    )?;
    let other = db.post("inbox", "ada", "caf\u{c9} @bo", &Default::default())?;
    let late = db.reply(
        root.id,
        "ada/tools",
        "CAF\u{c9} @bo/helper",
        &Default::default(),
    )?;
    let nonmatch = db.reply(other.id, "bob", "noise", &Default::default())?;
    db.post("inbox2", "ada", "caf\u{e9} @bo", &Default::default())?;
    let scope = Filter {
        channel: Some("inbox/".into()),
        by: Some("ada".into()),
        mentions: Some("@bo".into()),
        text: Some("CAF\u{c9}".into()),
        after: Some(0),
        ..Default::default()
    };
    let (next, rows) = db.threads_page(&scope, 1)?;
    assert_eq!(rows[0].message.id, root.id);
    assert_eq!(rows[0].last.as_ref().unwrap().id, late.id);
    assert_eq!(
        next, late.id,
        "a thread's position is its live activity, not its matching message"
    );
    let (head, rows) = db.threads_page(
        &Filter {
            after: Some(next),
            ..scope.clone()
        },
        1,
    )?;
    assert_eq!(rows[0].message.id, other.id);
    assert_eq!(rows[0].last.as_ref().unwrap().id, nonmatch.id);
    assert_eq!(head, db.cursor()?);
    let mut ids = vec![];
    let mut f = scope.clone();
    loop {
        let (next, rows) = db.messages_page(&f, 1)?;
        ids.extend(rows.iter().map(|m| m.id));
        if rows.is_empty() {
            break;
        }
        f.after = Some(next);
    }
    assert_eq!(ids, [root.id, other.id, late.id]);
    let watched = Filter {
        labels: BTreeMap::from([("stage".into(), "open|held".into())]),
        ..scope
    };
    let event = db.label(root.id, "ada", "stage", Some("done"))?;
    let matches = db
        .events(0, 100)?
        .into_iter()
        .filter(|e| e.matches(&watched))
        .map(|e| e.cursor)
        .collect::<Vec<_>>();
    assert_eq!(
        matches,
        [root.id, late.id, event.version],
        "root labels are frozen at delivery time; closing event matches before state"
    );
    assert!(db.messages(&watched, 100)?.is_empty());
    Ok(())
}

#[test]
fn older_windows_group_before_paging_and_keep_a_separate_snapshot() -> Result<()> {
    let db = Board::memory()?;
    let a = db.post("projects", "ada", "older root", &Default::default())?;
    let b = db.post("projects", "bob", "other thread", &Default::default())?;
    let c = db.post("projects", "ada", "newer thread", &Default::default())?;
    let last = db.reply(a.id, "bob", "new activity", &Default::default())?;
    let f = Filter::default();
    let head = db.threads_window(&f, None, 2)?;
    assert_eq!(
        head.rows.iter().map(|t| t.message.id).collect::<Vec<_>>(),
        [c.id, a.id]
    );
    let older = db.threads_window(&f, head.older, 2)?;
    assert_eq!(
        older.rows.iter().map(|t| t.message.id).collect::<Vec<_>>(),
        [b.id]
    );
    assert_eq!(older.older, None);
    let head = db.messages_window(&f, None, 2)?;
    assert_eq!(
        head.rows.iter().map(|m| m.id).collect::<Vec<_>>(),
        [c.id, last.id]
    );
    let older = db.messages_window(&f, head.older, 2)?;
    assert_eq!(
        older.rows.iter().map(|m| m.id).collect::<Vec<_>>(),
        [a.id, b.id]
    );
    let forward = Filter {
        after: Some(0),
        ..f
    };
    let page = db.threads_window(&forward, None, 1)?;
    assert_eq!(page.rows[0].message.id, b.id);
    assert_eq!(
        (page.cursor, page.snapshot, page.older),
        (b.id, last.id, None)
    );
    assert!(db.threads_window(&forward, Some(c.id), 1).is_err());
    Ok(())
}

#[test]
fn summaries_exclude_removed_replies_but_thread_reads_keep_and_validate_them() -> Result<()> {
    use board_local::errors::{Kind, classify};
    let b = Board::memory()?;
    let root = b.post("dev", "ann", "topic", &Default::default())?;
    let first = b.reply(root.id, "ann", "first", &Default::default())?;
    let unrelated = b.post("dev", "bob", "another topic", &Default::default())?;
    let second = b.reply(first.id, "ann", "second", &Default::default())?;
    let last = b.reply(root.id, "ann", "last", &Default::default())?;
    let f = Filter::default();
    let newest = b.conversation_window(first.id, &f, Some(2))?;
    assert_eq!(newest.message.id, root.id);
    assert_eq!(
        newest.replies.iter().map(|m| m.id).collect::<Vec<_>>(),
        [second.id, last.id]
    );
    let next = b.conversation_window(
        root.id,
        &Filter {
            after: Some(first.id),
            ..Default::default()
        },
        Some(1),
    )?;
    assert_eq!(next.replies[0].id, second.id);
    for cursor in [0, unrelated.id, i64::MAX] {
        let err = b
            .conversation_window(
                root.id,
                &Filter {
                    after: Some(cursor),
                    ..Default::default()
                },
                None,
            )
            .unwrap_err();
        assert_eq!(classify(&err), (Kind::Invalid, "invalid_after"));
    }
    b.delete(last.id, "ann", None)?;
    let rows = b.threads(&f, 10)?;
    assert_eq!(rows[1].message.id, root.id);
    assert_eq!(
        (rows[1].replies, rows[1].last.as_ref().map(|r| r.id)),
        (2, Some(second.id))
    );
    b.delete(second.id, "ann", None)?;
    let rows = b.threads(&f, 10)?;
    assert_eq!(
        rows[0].message.id, root.id,
        "live activity recedes behind the other root"
    );
    assert_eq!(
        (rows[0].replies, rows[0].last.as_ref().map(|r| r.id)),
        (1, Some(first.id))
    );
    b.delete(first.id, "ann", None)?;
    let rows = b.threads(&f, 10)?;
    assert_eq!((rows[0].replies, &rows[0].last), (0, &None));
    let tombstones = b.conversation_window(
        root.id,
        &Filter {
            after: Some(first.id),
            ..Default::default()
        },
        None,
    )?;
    assert!(tombstones.replies.iter().all(|m| m.deleted));
    assert_eq!(tombstones.replies.len(), 2);
    assert_eq!(
        b.channels()?[0].messages,
        5,
        "the channel counts posted messages, including removals"
    );
    Ok(())
}

#[test]
fn channel_and_schema_retries_do_not_duplicate_events_or_lose_authorship() -> Result<()> {
    let db = Board::memory()?;
    assert!(db.create_channel_as("empty", "ann")?);
    assert!(!db.create_channel_as("empty", "bob")?);
    let schema = json!({"type":"object"});
    assert!(db.register_schema_as("record", 1, "", &schema, "ann/assembler")?);
    assert!(!db.register_schema_as("record", 1, "", &schema, "bob")?);
    assert_eq!(db.actors()?, ["ann", "ann/assembler"]);
    let events = db.events(0, 100)?;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, "channel.created");
    assert_eq!(events[1].kind, "schema.registered");
    assert_eq!(events[0].actor, "ann");
    assert_eq!(events[1].actor, "ann/assembler");
    Ok(())
}

#[test]
fn simultaneous_first_open_initializes_one_database() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let n = 8;
    let gate = Barrier::new(n);
    for i in 0..4 {
        let path = dir.path().join(format!("{i}.db"));
        thread::scope(|s| -> Result<()> {
            let mut threads = vec![];
            for j in 0..n {
                let (gate, path) = (&gate, &path);
                threads.push(s.spawn(move || -> Result<()> {
                    gate.wait();
                    Board::open(path)?.post(
                        "first",
                        "writer",
                        &j.to_string(),
                        &Default::default(),
                    )?;
                    Ok(())
                }));
            }
            for thread in threads {
                thread.join().unwrap()?;
            }
            Ok(())
        })?;
        assert_eq!(Board::open(path)?.cursor()?, n as i64);
    }
    Ok(())
}

#[test]
fn opening_a_non_board_database_does_not_install_tables_or_change_its_journal() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("unrelated.db");
    let other = rusqlite::Connection::open(&path)?;
    other.execute_batch("CREATE TABLE notes(body TEXT); INSERT INTO notes VALUES('mine');")?;
    assert!(Board::open(&path).is_err());
    let journal: String = other.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
    assert_eq!(journal, "delete");
    let tables: Vec<String> = other
        .prepare("SELECT name FROM sqlite_schema WHERE type='table'")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    assert_eq!(tables, ["notes"]);
    Ok(())
}
