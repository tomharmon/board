use super::*;

#[test]
fn selected_task_has_a_scoped_guarded_reply_that_survives_restart() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let actor = "reader/ops";
    b.set_grant(
        "reader",
        None,
        actor,
        &board_local::access::Rules::parse("ops=read,post").unwrap(),
    )
    .unwrap();
    let token = b.issue_credential("reader", actor, None, None).unwrap();
    let mut scoped = Board::open(&f.db).unwrap();
    scoped.bind_token(&token.token).unwrap();
    let root = create_task(
        &b,
        "owner",
        TaskTarget::New {
            channel: "ops",
            text: "Review the release manifest",
            options: &PostOptions {
                files: vec![(
                    "checklist.txt".into(),
                    b"Verify the signed package".to_vec(),
                )],
                ..Default::default()
            },
        },
        &[],
        None,
    )
    .unwrap()
    .id;
    let seen = b
        .reply(root, "owner", "Ready to ship?", &Default::default())
        .unwrap()
        .id;
    let file = b.get(root).unwrap().attachments[0].id;
    let note = "Unrelated root draft";
    scoped
        .save_draft(
            actor,
            "new",
            Some(&board_local::Draft {
                channel: "ops".into(),
                body: note.into(),
                reply_to: None,
            }),
        )
        .unwrap();
    let path = f.dir.path().join("audit.txt");
    fs::write(&path, b"local audit").unwrap();
    let select = json!({"t":0.35,"type":"tap","key":format!("task-row-{root}")});
    let tasks = json!({"t":0.1,"type":"tap","key":"tasks"});
    let run = |steps: Value, end: &str| {
        let output = f
            .command(steps, DESKTOP, end)
            .env("BOARD_TOKEN", &token.token)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        f.tree()
    };
    let preview = run(
        json!([tasks,select,{"t":0.65,"type":"tap","key":format!("preview-{root}-{file}")}]),
        "1.0",
    );
    preview.expect_key("preview-text");
    preview.expect_text("Verify the signed package");

    let mut cmd = f.command(json!([
        tasks,select,{"t":0.65,"type":"tap","key":format!("inspect-{seen}")},
        {"t":0.9,"type":"tap","key":"guard-reply"},{"t":1.15,"paste":"Audit passed"},
        {"t":1.45,"type":"tap","key":"composer-private"},{"t":1.75,"type":"tap","key":"attach-file"},
        {"t":2.05,"type":"file-dialog","path":path},{"t":2.4,"type":"mark","name":"reviewing"},
        {"t":3.95,"type":"tap","key":"send"}
    ]), DESKTOP, "4.3");
    cmd.env("BOARD_TOKEN", &token.token);
    let conflict = f.at_mark(&mut cmd, "reviewing", || {
        assert!(b.typing(root).unwrap().iter().any(|p| p.actor == actor));
        b.reply(seen, "owner", "Hold for audit", &Default::default())
            .unwrap();
    });
    let messages = b.thread(root).unwrap();
    assert_eq!(messages.len(), 3);
    let latest = messages.last().unwrap().id;
    conflict.expect_text(&format!(
        "newer_message: latest message is #{latest}, not #{seen}"
    ));
    conflict.expect_key("task-detail-scroll");
    conflict.expect_key("reply-composer");
    assert!(!conflict.has_key("open-task"));
    let scope = format!("thread:{root}");
    assert!(b.load_composer(actor, &scope).unwrap().is_none());
    let saved = scoped.load_composer(actor, &scope).unwrap().unwrap();
    assert_eq!(saved.draft.reply_to, Some(seen));
    assert_eq!(saved.settings.after, Some(seen));
    assert!(saved.settings.no_sync);
    assert_eq!(
        saved.files,
        vec![("audit.txt".into(), b"local audit".to_vec())]
    );

    let sent = run(
        json!([
            tasks,select,{"t":0.6,"type":"tap","key":"message-body"},{"t":0.8,"type":"key","name":"Escape"},
            {"t":1.05,"type":"tap","key":"task-options"},{"t":1.3,"type":"key","name":"Escape"},
            {"t":1.55,"type":"tap","key":format!("inspect-{latest}")},{"t":1.85,"type":"tap","key":"guard-reply"},
            {"t":2.1,"type":"tap","key":"send"}
        ]),
        "2.5",
    );
    sent.expect_key("task-detail-scroll");
    sent.expect_key("reply-composer");
    sent.expect_text("Audit passed");
    let messages = b.thread(root).unwrap();
    assert_eq!(messages.len(), 4);
    let reply = messages.last().unwrap();
    assert_eq!(
        (reply.actor.as_str(), reply.reply_to),
        (actor, Some(latest))
    );
    assert!(reply.no_sync);
    assert_eq!(
        b.download(reply.attachments[0].id).unwrap().1,
        b"local audit"
    );
    assert!(scoped.load_composer(actor, &scope).unwrap().is_none());
    assert_eq!(scoped.load_draft(actor, "new").unwrap().unwrap().body, note);
    assert!(b.typing(root).unwrap().is_empty());
}
