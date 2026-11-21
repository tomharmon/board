use super::*;

#[test]
fn a_stale_task_reply_does_not_prevent_the_other_panes_task_draft_from_surviving_restart() {
    use board_local::{ComposerSettings, Draft};
    let f = Fixture::new();
    let db = Board::open(&f.db).unwrap();
    let id = create_task(
        &db,
        "owner",
        TaskTarget::New {
            channel: "ops",
            text: "Coordinate release",
            options: &Default::default(),
        },
        &[],
        None,
    )
    .unwrap()
    .id;
    let scope = format!("thread:{id}");
    let mut reply = Draft {
        body: "Initial reply".into(),
        channel: "ops".into(),
        reply_to: Some(id),
    };
    let settings = ComposerSettings::default();
    let stamp = db
        .save_composer("reader", &scope, None, Some((&reply, &settings)), None)
        .unwrap()
        .unwrap();
    let file = f.dir.path().join("checklist.txt");
    fs::write(&file, "local draft attachment").unwrap();
    let mut cmd = f.command(
        json!([
            {"t":0.1,"type":"tap","key":"tasks"},{"t":0.4,"type":"tap","key":format!("task-row-{id}")},
            {"t":0.7,"type":"mark","name":"reply-open"},{"t":1.2,"type":"tap","key":"message-body"},
            {"t":1.45,"type":"key","name":"End"},{"t":1.65,"paste":" edited in this window"},
            {"t":2.05,"type":"tap","key":"quick-task-channel"},{"t":2.25,"paste":"ops"},
            {"t":2.5,"type":"tap","key":"quick-task-body"},{"t":2.7,"paste":"Independent task"},
            {"t":3.0,"type":"tap","key":"quick-task-local"},{"t":3.2,"type":"tap","key":"quick-task-attach"},
            {"t":3.45,"type":"file-dialog","path":file}
        ]),
        DESKTOP,
        "4.5",
    );
    let tree = f.at_mark(&mut cmd, "reply-open", || {
        reply.body = "Reply from the other window".into();
        db.save_composer(
            "reader",
            &scope,
            Some(&stamp),
            Some((&reply, &settings)),
            None,
        )
        .unwrap();
    });
    tree.expect_key("reply-composer");
    tree.expect_key("task-composer");
    assert!(tree.0.iter().any(|el| {
        el["text"]
            .as_str()
            .is_some_and(|s| s.contains("Draft changed in another window"))
    }));
    drop(db);
    let reopened = Board::open(&f.db).unwrap();
    assert_eq!(
        reopened
            .load_composer("reader", &scope)
            .unwrap()
            .unwrap()
            .draft
            .body,
        reply.body
    );
    let saved = reopened
        .load_composer("reader", "tasks")
        .unwrap()
        .expect("independent task draft was not saved");
    assert_eq!(saved.draft.body, "Independent task");
    assert!(saved.settings.no_sync);
    assert_eq!(
        saved.files,
        [("checklist.txt".into(), b"local draft attachment".to_vec())]
    );
    assert_eq!(reopened.thread(id).unwrap().len(), 1);
}

#[test]
fn losing_a_task_drafts_destination_hides_staged_bytes_without_erasing_the_sealed_copy() {
    use board_local::{ComposerSettings, Draft, TaskDraftSettings, access::Rules};
    let f = Fixture::new();
    let admin = Board::open(&f.db).unwrap();
    let actor = "reader/ops";
    let grant = admin
        .set_grant(
            "reader",
            None,
            actor,
            &Rules::parse("ops=read,post;public=read,post").unwrap(),
        )
        .unwrap();
    let token = admin
        .issue_credential("reader", actor, None, None)
        .unwrap()
        .token;
    let mut scoped = Board::open(&f.db).unwrap();
    scoped.bind_token(&token).unwrap();
    let draft = Draft {
        channel: "ops".into(),
        body: "Private task not yet sent".into(),
        reply_to: None,
    };
    let settings = ComposerSettings {
        task: Some(TaskDraftSettings::default()),
        no_sync: true,
        ..Default::default()
    };
    scoped
        .save_composer(
            actor,
            "tasks",
            None,
            Some((&draft, &settings)),
            Some(&[("private-payload.txt".into(), b"private".to_vec())]),
        )
        .unwrap();
    let mut cmd=f.command(json!([{"t":0.12,"type":"tap","key":"tasks"},{"t":0.5,"type":"mark","name":"task-visible"}]),DESKTOP,"2.9");
    cmd.env("BOARD_TOKEN", &token);
    let t = f.at_mark(&mut cmd, "task-visible", || {
        admin
            .set_grant(
                "reader",
                Some(grant.id),
                actor,
                &Rules::parse("public=read,post").unwrap(),
            )
            .unwrap();
    });
    t.expect_key("task-composer");
    t.expect_text("This saved task draft is no longer visible to the active credential");
    assert!(
        !t.0.iter()
            .any(|e| e.to_string().contains("Private task not yet sent")
                || e.to_string().contains("private-payload.txt"))
    );
    assert!(scoped.load_composer(actor, "tasks").is_err());
    admin
        .set_grant(
            "reader",
            Some(grant.id),
            actor,
            &Rules::parse("ops=read,post").unwrap(),
        )
        .unwrap();
    assert_eq!(
        scoped.load_composer(actor, "tasks").unwrap().unwrap().files[0].1,
        b"private"
    );
}

#[test]
fn scrolling_to_the_oldest_loaded_row_prepends_history_without_losing_the_reading_position() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    for n in 1..=64 {
        b.post(
            "notes",
            "person",
            &format!("Chronological note {n}"),
            &Default::default(),
        )
        .unwrap();
    }
    let select = json!({"t":0.12,"type":"tap","key":"nav-notes"});
    let first = f.render(json!([select]), DESKTOP, "0.65");
    first.expect_key("thread-15");
    first.expect_key("thread-64");
    assert!(!first.has_key("thread-1"));
    let more = f.render(
        json!([select,{"t":0.55,"type":"wheel","x":550,"y":270,"dy":18000}]),
        DESKTOP,
        "1.4",
    );
    assert!(
        more.has_key("thread-1"),
        "first row {:?}, viewport {:?}",
        more.bounds("thread-15"),
        more.bounds("feed-scroll")
    );
    more.expect_key("thread-64");
    let anchor = more.bounds("thread-15");
    let window = more.bounds("feed-scroll");
    assert!(
        anchor.y + anchor.h >= window.y - 2. && anchor.y < window.bottom(),
        "prepended rows moved the reader away: {anchor:?} {window:?}"
    );
}

#[test]
fn scoped_inline_mentions_and_label_suggestions_apply_exact_visible_values() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    for (channel, author, phase) in [
        ("ops", "sam-visible", "review"),
        ("staff", "sam-private", "secret-internal"),
    ] {
        b.post(
            channel,
            author,
            "A labeled message",
            &PostOptions {
                labels: [("phase".into(), phase.into())].into(),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let actor = "reader/ops";
    b.set_grant(
        "reader",
        None,
        actor,
        &board_local::access::Rules::parse("ops=read,post,label").unwrap(),
    )
    .unwrap();
    let secret = b
        .issue_credential("reader", actor, None, None)
        .unwrap()
        .token;
    let run = |steps: Value, end: &str| {
        let out = f
            .command(steps, DESKTOP, end)
            .env("BOARD_TOKEN", &secret)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        f.tree()
    };
    let t = run(
        json!([
            {"t":0.1,"type":"tap","key":"nav-ops"},{"t":0.35,"type":"tap","key":"message-body"},{"t":0.6,"paste":"Thanks @sam"}
        ]),
        "1.0",
    );
    t.expect_key("mention-choice-0");
    assert!(!t.has_key("mention-choice-1"));
    assert!(!t.0.iter().any(|e| e.to_string().contains("sam-private")));
    let t = run(
        json!([
            {"t":0.1,"type":"tap","key":"nav-ops"},{"t":0.35,"type":"tap","key":"message-body"},
            {"t":0.5,"type":"key","name":"End"},{"t":0.7,"paste":"-v"},{"t":0.95,"type":"key","name":"Tab"},
            {"t":1.2,"type":"tap","key":"delivery-options"},{"t":1.45,"type":"tap","key":"compose-labels"},{"t":1.7,"paste":"ph"}
        ]),
        "2.0",
    );
    t.expect_key("compose-labels-suggest-0");
    t.expect_text("phase=review");
    assert!(
        !t.0.iter()
            .any(|e| e.to_string().contains("secret-internal"))
    );
    let t = run(
        json!([
            {"t":0.1,"type":"tap","key":"nav-ops"},{"t":0.4,"type":"tap","key":"delivery-options"},
            {"t":0.65,"type":"tap","key":"compose-labels"},{"t":0.85,"type":"key","name":"End"},{"t":1.1,"type":"tap","key":"compose-labels-suggest-0"},
            {"t":1.35,"type":"tap","key":"done-delivery"},{"t":1.6,"type":"tap","key":"send"}
        ]),
        "1.9",
    );
    t.expect_text("phase=review");
    let rows = b
        .messages(
            &Filter {
                channel: Some("ops".into()),
                ..Default::default()
            },
            10,
        )
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].text.trim_end(), "Thanks @[sam-visible]");
    assert_eq!(rows[1].labels["phase"], "review");
    let filtered = run(
        json!([
            {"t":0.1,"type":"tap","key":"nav-ops"},{"t":0.35,"type":"tap","key":"filter-messages"},
            {"t":0.6,"type":"tap","key":"filter-labels"},{"t":0.8,"paste":"phase=other|r"},
            {"t":1.0,"type":"tap","key":"filter-labels-suggest-0"},{"t":1.3,"type":"tap","key":"apply-filters"}
        ]),
        "1.6",
    );
    filtered.expect_key(&format!("thread-{}", rows[1].id));
    assert!(filtered.0.iter().any(|e| {
        e["text"]
            .as_str()
            .is_some_and(|s| s.contains("phase=other|review"))
    }));
}

#[test]
fn composer_formats_a_selection_continues_lists_and_up_edits_only_own_visible_message() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    b.post("work", "owner", "Meeting", &Default::default())
        .unwrap();
    let t=f.render(json!([
        {"t":0.1,"type":"tap","key":"nav-work"},{"t":0.3,"type":"tap","key":"message-body"},
        {"t":0.5,"paste":"café"},{"t":0.7,"type":"key","cmd":true,"name":"a"},{"t":0.9,"type":"key","cmd":true,"name":"b"},
        {"t":1.1,"type":"key","cmd":true,"name":"i"},{"t":1.3,"type":"key","cmd":true,"name":"e"},{"t":1.5,"type":"key","name":"Enter"},
        {"t":1.8,"type":"tap","key":"nav-work"},{"t":2.0,"type":"tap","key":"message-body"},{"t":2.2,"paste":"  9. first"},
        {"t":2.4,"type":"key","name":"Enter"},{"t":2.6,"paste":"second"},{"t":2.8,"type":"key","name":"Enter"},
        {"t":3.0,"type":"key","name":"Enter"},{"t":3.2,"paste":"- last"},{"t":3.4,"type":"key","cmd":true,"name":"Enter"}
    ]),DESKTOP,"3.7");
    t.expect_key("reply-composer");
    let rows = b.messages(&Filter::default(), 10).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[1].text, "**_`café`_**");
    assert_eq!(rows[2].text, "  9. first\n  10. second\n- last");
    let other = b
        .post(
            "work",
            "owner",
            "Someone else posted next",
            &Default::default(),
        )
        .unwrap();
    let other_channel = b
        .post(
            "elsewhere",
            "reader",
            "My newer message elsewhere",
            &Default::default(),
        )
        .unwrap();
    let t=f.render(json!([
        {"t":0.1,"type":"tap","key":"nav-work"},{"t":0.3,"type":"tap","key":"message-body"},{"t":0.5,"type":"key","name":"Up"},
        {"t":0.85,"type":"key","cmd":true,"name":"a"},{"t":1.1,"paste":"Corrected list"},{"t":1.4,"type":"tap","key":"save-edit"}
    ]),DESKTOP,"1.8");
    t.expect_text("Corrected list");
    assert_eq!(b.get(rows[2].id).unwrap().text, "Corrected list");
    assert_eq!(b.get(other.id).unwrap().text, other.text);
    assert_eq!(b.get(other_channel.id).unwrap().text, other_channel.text);
}

#[test]
fn channel_activity_and_task_drafts_reopen_independently_with_private_bytes_and_routing() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let prior = create_task(
        &b,
        "owner",
        TaskTarget::New {
            channel: "ops",
            text: "Build",
            options: &PostOptions::default(),
        },
        &[],
        None,
    )
    .unwrap()
    .id;
    b.post("notes", "owner", "Another channel", &Default::default())
        .unwrap();
    let file = f.dir.path().join("manifest.txt");
    fs::write(&file, "private manifest").unwrap();
    f.render(json!([
        {"t":0.1,"type":"tap","key":"message-body"},{"t":0.3,"paste":"Activity scratch"},
        {"t":0.5,"type":"tap","key":"nav-notes"},{"t":0.7,"type":"tap","key":"message-body"},{"t":0.9,"paste":"Private notes"},{"t":1.1,"type":"tap","key":"composer-private"},
        {"t":1.35,"type":"tap","key":"nav-ops"},{"t":1.55,"type":"tap","key":"message-body"},{"t":1.75,"paste":"Ops update"},
        {"t":2.0,"type":"tap","key":"tasks"},{"t":2.2,"type":"tap","key":"new-task"},{"t":2.45,"type":"tap","key":"task-channel"},
        {"t":2.65,"type":"key","cmd":true,"name":"a"},{"t":2.85,"paste":"ops"},{"t":3.05,"type":"tap","key":"task-body"},{"t":3.25,"paste":"Verify manifest"},
        {"t":3.45,"type":"tap","key":"assignee"},{"t":3.65,"paste":"reader/agent"},{"t":3.85,"type":"tap","key":"task-needs"},{"t":4.05,"paste":format!("#{prior}")},
        {"t":4.25,"type":"tap","key":"task-labels"},{"t":4.45,"paste":"phase=verify"},{"t":4.65,"type":"tap","key":"task-local"},
        {"t":4.85,"type":"tap","key":"task-attach"},{"t":5.05,"type":"file-dialog","path":file},{"t":5.35,"type":"tap","key":"nav-ops"}
    ]),DESKTOP,"5.7");
    for (scope, body, private) in [
        ("new", "Activity scratch", false),
        ("channel:notes", "Private notes", true),
        ("channel:ops", "Ops update", false),
        ("tasks", "Verify manifest", true),
    ] {
        let d = b.load_composer("reader", scope).unwrap().unwrap();
        assert_eq!(d.draft.body, body, "{scope}");
        assert_eq!(d.settings.no_sync, private, "{scope}");
    }
    let mut cmd=f.command(json!([
        {"t":0.1,"type":"tap","key":"tasks"},{"t":0.5,"type":"mark","name":"restored"},{"t":2.2,"type":"tap","key":"quick-task-send"}
    ]),DESKTOP,"2.6");
    let t = f.at_mark(&mut cmd, "restored", || {
        let mut saved = b.load_composer("reader", "tasks").unwrap().unwrap();
        assert_eq!(saved.files[0].1, b"private manifest");
        assert_eq!(
            saved
                .settings
                .task
                .as_ref()
                .unwrap()
                .parsed_needs()
                .unwrap(),
            vec![prior]
        );
        saved.draft.body = "Updated by the second window".into();
        b.save_composer(
            "reader",
            "tasks",
            Some(&saved.stamp),
            Some((&saved.draft, &saved.settings)),
            None,
        )
        .unwrap();
    });
    t.expect_key("reload-task-draft");
    assert_eq!(
        board_local::automation::list_tasks(&b, None, Some("all"))
            .unwrap()
            .len(),
        1
    );
    let t = f.render(
        json!([
            {"t":0.1,"type":"tap","key":"tasks"},{"t":0.45,"type":"tap","key":"quick-task-send"}
        ]),
        DESKTOP,
        "0.9",
    );
    t.expect_text("Updated by the second window");
    let tasks = board_local::automation::list_tasks(&b, None, Some("all")).unwrap();
    let new = tasks.iter().find(|v| v.id != prior).unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(new.needs, vec![prior]);
    assert_eq!(new.assignee.as_deref(), Some("reader/agent"));
    let msg = b.get(new.id).unwrap();
    assert!(msg.no_sync);
    assert_eq!(msg.labels["phase"], "verify");
    assert_eq!(
        b.download(msg.attachments[0].id).unwrap().1,
        b"private manifest"
    );
    assert!(b.load_composer("reader", "tasks").unwrap().is_none());
    let roots = f.render(
        json!([{"t":0.2,"type":"tap","key":"nav-notes"}]),
        DESKTOP,
        "0.65",
    );
    roots.expect_text("Private notes");
    roots.expect_key("draft-local-only");
    assert!(
        !roots
            .0
            .iter()
            .any(|n| n["text"] == "Ops update" || n["text"] == "Activity scratch")
    );
}
