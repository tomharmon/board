use super::*;

fn examples(f: &Fixture) -> [i64; 3] {
    let b = Board::open(&f.db).unwrap();
    b.post(
        "general",
        "mira",
        "Welcome to the local board. Post handoffs and questions here.",
        &Default::default(),
    )
    .unwrap();
    let root = b
        .post(
            "engineering",
            "rowan",
            "The build cache is ready for review.",
            &PostOptions {
                labels: [("phase".into(), "review".into())].into(),
                ..Default::default()
            },
        )
        .unwrap();
    b.post(
        "engineering/build",
        "lin",
        "Linux and macOS checks passed on the new settings.",
        &Default::default(),
    )
    .unwrap();
    b.reply(
        root.id,
        "mira",
        "I checked the cold run. The cache reduced build time to 42 seconds.",
        &Default::default(),
    )
    .unwrap();
    b.post(
        "general",
        "rowan",
        "Please leave any final notes before the release.",
        &Default::default(),
    )
    .unwrap();
    let task = |actor, ch, txt, needs: &[i64], labels| {
        create_task(
            &b,
            actor,
            TaskTarget::New {
                channel: ch,
                text: txt,
                options: &PostOptions {
                    labels,
                    ..Default::default()
                },
            },
            needs,
            None,
        )
        .unwrap()
        .id
    };
    let first = task(
        "lin",
        "engineering",
        "Verify installers on a clean machine",
        &[],
        [("phase".into(), "release".into())].into(),
    );
    [
        first,
        task(
            "lin",
            "engineering",
            "Publish the release notes",
            &[first],
            Default::default(),
        ),
        task(
            "mira",
            "general",
            "Review the public checklist",
            &[],
            Default::default(),
        ),
    ]
}

#[test]
fn compact_task_table_and_settings_tabs_stay_usable_at_desktop_and_phone_sizes() {
    let f = Fixture::new();
    let [first, second, last] = examples(&f);
    let b = Board::open(&f.db).unwrap();
    b.set_grant(
        "reader",
        None,
        "reader/scan",
        &board_local::access::Rules::parse("engineering/=read,react").unwrap(),
    )
    .unwrap();
    let nav = json!({"t":0.1,"type":"tap","key":"open-nav"});
    let tasks = json!({"t":0.35,"type":"tap","key":"tasks"});
    let settings = json!({"t":0.35,"type":"tap","key":"workspace"});
    for (size, prefix, steps) in [
        ((1200, 800), "desktop", vec![]),
        ((390, 844), "phone", vec![nav.clone()]),
    ] {
        let taps = |end: &Value| Value::Array(steps.iter().cloned().chain([end.clone()]).collect());
        let (table, frame) = f.snapshot(taps(&tasks), size, "light", &format!("{prefix}-tasks"));
        for key in [
            "segment0",
            "task-state-waiting",
            "segment1",
            "segment2",
            "task-state-blocked",
            "task-state-completed",
            "task-state-failed",
            "task-state-canceled",
            "segment3",
        ] {
            let r = table.bounds(key);
            assert!(
                r.x >= 0.0 && r.edge() <= size.0 as f64 + 1.0 && r.y < 155.0,
                "offscreen state {key}: {r:?}"
            );
        }
        let rows = [first, second, last].map(|id| table.bounds(&format!("task-row-{id}")));
        close_to(rows[0].h, 30.0, 1.0, "task row height");
        assert!(rows[0].y < rows[1].y && rows[1].y < rows[2].y);
        let composer = table.bounds("task-composer");
        close_to(
            composer.x,
            if prefix == "desktop" { 244.0 } else { 20.0 },
            3.0,
            "task composer inset",
        );
        close_to(
            composer.bottom(),
            size.1 as f64 - 20.0,
            2.0,
            "task composer bottom",
        );
        assert!(composer.y > rows[2].bottom());
        assert_eq!(frame.dimensions(), size);
        let (tabs, _) = f.snapshot(
            taps(&settings),
            size,
            "light",
            &format!("{prefix}-settings"),
        );
        for key in [
            "work-access",
            "work-schemas",
            "work-schedules",
            "work-watchers",
            "work-sources",
            "work-profiles",
            "work-history",
            "add-resource",
        ] {
            let r = tabs.bounds(key);
            assert!(
                r.x >= 0.0 && r.edge() <= size.0 as f64 + 1.0,
                "unreachable settings {key}: {r:?}"
            );
        }
        tabs.expect_text("Grant #1    reader/scan");
        tabs.expect_key("explain-access");
    }
    let (detail, _) = f.snapshot(
        json!([tasks,{"t":0.6,"type":"tap","key":format!("task-row-{second}")}]),
        (1200, 800),
        "light",
        "desktop-task-detail",
    );
    assert!(detail.bounds("task-board").edge() <= detail.bounds("task-detail-scroll").x + 2.0);
    detail.expect_key("task-options");
    detail.expect_key("reply-composer");
    detail.expect_key(&format!("message-{second}"));
    detail.expect_text("0 replies");
    let (detail, _) = f.snapshot(
        json!([nav,tasks,{"t":0.65,"type":"tap","key":format!("task-row-{second}")}]),
        (390, 844),
        "light",
        "phone-task-detail",
    );
    assert!(!detail.has_key("task-board"));
    detail.expect_key("task-options");
    detail.expect_key("reply-composer");
    detail.expect_key(&format!("message-{second}"));
}

#[test]
fn task_conversation_posts_a_reply_without_leaving_the_table() {
    let f = Fixture::new();
    let [first, _, _] = examples(&f);
    let b = Board::open(&f.db).unwrap();
    let tree=f.render(json!([
        {"t":0.1,"type":"tap","key":"tasks"},{"t":0.3,"type":"tap","key":format!("task-row-{first}")},
        {"t":0.55,"type":"tap","key":"message-body"},{"t":0.8,"paste":"Package checks passed on a clean install."},
        {"t":1.05,"type":"tap","key":"send"}
    ]),DESKTOP,"1.4");
    let messages = b.thread(first).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].re, Some(first));
    assert_eq!(
        messages[1].text,
        "Package checks passed on a clean install."
    );
    tree.expect_key(&format!("message-{first}"));
    tree.expect_key(&format!("message-{}", messages[1].id));
    tree.expect_key("task-board");
    tree.expect_key("task-banner");
    tree.expect_key("reply-composer");
    tree.expect_key("task-options");
}

#[test]
fn quick_task_keeps_list_open_and_applies_labels_privacy_and_local_file() {
    let f = Fixture::new();
    let file = f.dir.path().join("notes.txt");
    fs::write(&file, "trusted local bytes").unwrap();
    let t=f.render(json!([
        {"t":0.15,"type":"tap","key":"tasks"},{"t":0.4,"type":"tap","key":"quick-task-channel"},{"t":0.55,"paste":"#engineering"},
        {"t":0.75,"type":"tap","key":"quick-task-body"},{"t":0.95,"paste":"Review the signed package"},
        {"t":1.1,"type":"tap","key":"quick-task-local"},{"t":1.3,"type":"tap","key":"quick-task-attach"},{"t":1.5,"type":"file-dialog","path":file},
        {"t":1.7,"type":"tap","key":"quick-task-tags"},{"t":1.95,"paste":"phase=release"},
        {"t":2.15,"type":"tap","key":"quick-task-send"}
    ]),DESKTOP,"2.5");
    let b = Board::open(&f.db).unwrap();
    let rows = board_local::automation::list_tasks(&b, None, Some("all")).unwrap();
    assert_eq!(rows.len(), 1);
    let m = b.get(rows[0].id).unwrap();
    assert_eq!(
        (&m.text[..], &m.channel[..], &m.labels["phase"][..]),
        ("Review the signed package", "engineering", "release")
    );
    assert!(m.no_sync);
    assert_eq!(m.attachments[0].name, "notes.txt");
    assert_eq!(
        b.download(m.attachments[0].id).unwrap().1,
        b"trusted local bytes"
    );
    t.expect_key("task-composer");
    t.expect_key(&format!("task-row-{}", m.id));
    assert!(!t.has_key("task-file-0"));
}

#[test]
fn settings_explainer_reads_local_policy_without_showing_registry_to_scoped_token() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    b.set_grant(
        "reader",
        None,
        "reader/scan",
        &board_local::access::Rules::parse("engineering/=read,react").unwrap(),
    )
    .unwrap();
    b.set_grant(
        "reader",
        None,
        "another-secret-identity",
        &board_local::access::Rules::parse("staff=read").unwrap(),
    )
    .unwrap();
    let token = b
        .issue_credential(
            "reader",
            "reader/scan",
            Some(&board_local::access::Rules::parse("engineering/=read").unwrap()),
            None,
        )
        .unwrap();
    let base = json!([{"t":0.1,"type":"tap","key":"workspace"},{"t":0.35,"type":"tap","key":"explain-channel"},{"t":0.55,"paste":"#engineering/build"}]);
    let mut steps = base.as_array().unwrap().clone();
    steps.push(json!({"t":0.75,"type":"tap","key":"explain-access"}));
    let res = f
        .command(Value::Array(steps), DESKTOP, "1.05")
        .env("BOARD_TOKEN", token.token)
        .output()
        .unwrap();
    assert!(
        res.status.success(),
        "{}",
        String::from_utf8_lossy(&res.stderr)
    );
    let tree = f.tree();
    tree.expect_text("@reader/scan  #engineering/build  read");
    for key in ["explain-subject", "add-resource", "remove-grant-2"] {
        assert!(!tree.has_key(key));
    }
    assert!(
        !tree
            .0
            .iter()
            .any(|e| e.to_string().contains("another-secret-identity"))
    );
    let mut steps = base.as_array().unwrap().clone();
    steps.extend([
        json!({"t":0.75,"type":"tap","key":"explain-subject"}),
        json!({"t":1.0,"paste":"reader/scan"}),
        json!({"t":1.25,"type":"tap","key":"explain-access"}),
    ]);
    let tree = f.render(Value::Array(steps), DESKTOP, "1.5");
    assert!(tree.0.iter().any(|e| {
        e["key"] == "access-explanation"
            && e["text"]
                .as_str()
                .is_some_and(|s| s.starts_with("@reader/scan  #engineering/build  read react"))
    }));
}

#[test]
fn delayed_task_picker_does_not_read_a_file_into_a_replacement_entry() {
    let f = Fixture::new();
    let source = f.dir.path().join("not-on-disk");
    let tree=f.render(json!([
        {"t":0.1,"type":"tap","key":"tasks"},{"t":0.3,"type":"tap","key":"quick-task-channel"},{"t":0.5,"paste":"ops"},
        {"t":0.7,"type":"tap","key":"quick-task-body"},{"t":0.9,"paste":"First task"},
        {"t":1.1,"type":"tap","key":"quick-task-attach"},{"t":1.35,"type":"tap","key":"quick-task-send"},
        {"t":1.55,"type":"tap","key":"quick-task-channel"},{"t":1.75,"paste":"ops"},
        {"t":1.95,"type":"file-dialog","path":source}
    ]),DESKTOP,"2.3");
    tree.expect_text("Task entry changed; file was not attached");
    assert!(!tree.has_key("task-file-0"));
    let b = Board::open(&f.db).unwrap();
    let rows = board_local::automation::list_tasks(&b, None, Some("all")).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(b.get(rows[0].id).unwrap().attachments.is_empty());
}

#[test]
fn task_picker_checks_live_channel_permission_before_opening_local_bytes() {
    use board_local::access::Rules;
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let actor = "reader/ops";
    let grant = b
        .set_grant(
            "reader",
            None,
            actor,
            &Rules::parse("ops=read,post").unwrap(),
        )
        .unwrap();
    let token = b.issue_credential("reader", actor, None, None).unwrap();
    let mut cmd=f.command(json!([
        {"t":0.1,"type":"tap","key":"tasks"},{"t":0.3,"type":"tap","key":"quick-task-channel"},{"t":0.5,"paste":"ops"},
        {"t":0.7,"type":"tap","key":"quick-task-attach"},{"t":0.95,"type":"mark","name":"task-choosing"},
        {"t":2.8,"type":"file-dialog","path":f.dir.path().join("not-on-disk")}
    ]),DESKTOP,"3.1");
    cmd.env("BOARD_TOKEN", token.token);
    let tree = f.at_mark(&mut cmd, "task-choosing", || {
        b.set_grant(
            "reader",
            Some(grant.id),
            actor,
            &Rules::parse("ops=read").unwrap(),
        )
        .unwrap();
    });
    tree.expect_text("Posting to this channel is no longer permitted");
    assert!(!tree.has_key("task-file-0"));
}
