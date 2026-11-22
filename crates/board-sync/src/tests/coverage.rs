use super::*;

fn provider_fields(db: &Board, source: &str, thread: &str, fields: &[(&str, &str)]) {
    let mut signal = upsert(
        thread,
        "metadata".into(),
        "provider".into(),
        None,
        String::new(),
    );
    signal.change = Change::Signal {
        conditions: BTreeMap::new(),
        set: fields.iter().map(|&(k, v)| (k.into(), v.into())).collect(),
        unset: vec![],
        reply: None,
    };
    db.enqueue_imports("p", source, &[envelope(signal, Value::Null)], None)
        .unwrap();
    assert_eq!(db.drain_source("p", source, 5).unwrap(), (1, 0));
}

#[test]
fn empty_as_tasks_pull_drains_an_interrupted_ordinary_checkpoint_as_claimable_work() {
    let mock = Mock::new(vec![
        ("GET /repos/org/proj/issues?", 200, json!([])),
        ("GET /repos/org/proj/issues/comments?", 200, json!([])),
        ("GET /repos/org/proj/pulls/comments?", 200, json!([])),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board");
    let b = Board::open(&path).unwrap();
    b.create_source("ada", "bugs", Provider::Github, "org/proj", "issues")
        .unwrap();
    let mut head = upsert(
        "17",
        "issue".into(),
        "reporter".into(),
        None,
        "Pending issue".into(),
    );
    labels(&mut head, &[("tag.bug", "bug"), ("state", "open")]);
    let comment = upsert(
        "17",
        "comment:7".into(),
        "reporter".into(),
        Some("issue".into()),
        "Reproduced".into(),
    );
    b.enqueue_imports(
        "ada",
        "bugs",
        &[envelope(head, json!({})), envelope(comment, json!({}))],
        Some(&Checkpoint {
            stream: "last_scan".into(),
            expected: None,
            next: "2026-01-01T00:00:00Z".into(),
        }),
    )
    .unwrap();
    drop(b);
    let b = Board::open(&path).unwrap();
    let c = Connector::new(Provider::Github, mock.base.clone(), Some("test".into())).unwrap();
    let r = c.pull_with_tasks(&b, "ada", "bugs", None, true).unwrap();
    assert_eq!((r.records, r.threads, r.applied, r.failed), (0, 0, 2, 0));
    let m = b.find_source("ada", "bugs", "17", None).unwrap().unwrap();
    let t = m.task.expect("a newly applied backlog root must be a task");
    assert_eq!((t.state.as_str(), t.by.as_str()), ("ready", "ada"));
    assert_eq!(stored_fields(&b, m.id)["tag.bug"], "bug");
    assert!(
        b.find_source("ada", "bugs", "17", Some("comment:7"))
            .unwrap()
            .unwrap()
            .task
            .is_none()
    );
    assert!(
        mock.finish()
            .iter()
            .all(|(request, _)| request.starts_with("GET "))
    );
}

#[test]
fn github_pull_as_tasks_inherits_provider_labels_and_claim_survives_refetch() {
    use board_local::automation::{claim, get_task};
    let issue = json!({"number":1,"state":"open","title":"Lost receipt","body":"steps","user":{"id":1},
        "html_url":"https://github.com/org/proj/issues/1","labels":[{"name":"bug"}]});
    let comment = json!([{"id":7,"body":"confirmed","user":{"id":2},"html_url":"https://github.com/org/proj/issues/1#issuecomment-7"}]);
    let fetch = vec![
        ("GET /repos/org/proj/issues/1 ", 200, issue),
        ("GET /repos/org/proj/issues/1/comments?", 200, comment),
    ];
    let mock = Mock::new([fetch.clone(), fetch].concat());
    let (b, c) = setup(Provider::Github, "org/proj", &mock);
    let r = c
        .pull_with_tasks(&b, "ada", "test", Some("1"), true)
        .unwrap();
    assert_eq!((r.records, r.applied, r.failed), (2, 2, 0));
    let root = b.find_source("ada", "test", "1", None).unwrap().unwrap();
    assert_eq!(root.task.as_ref().unwrap().state, "ready");
    let fields = stored_fields(&b, root.id);
    assert_eq!(fields["state"], "open");
    assert!(fields.contains_key("tag.bug"));
    let t = claim(&b, "ada/dev", root.id, 60).unwrap();
    assert_eq!(
        c.pull_with_tasks(&b, "ada", "test", Some("1"), true)
            .unwrap()
            .applied,
        0
    );
    assert_eq!(get_task(&b, root.id).unwrap().epoch, t.epoch);
    assert!(
        b.find_source("ada", "test", "1", Some("comment:7"))
            .unwrap()
            .unwrap()
            .task
            .is_none()
    );
    mock.finish();
}

#[test]
fn github_routes_new_issues_and_pulls_separately_without_disconnecting_an_existing_thread() {
    let b = Board::memory().unwrap();
    for (name, kind) in [
        ("features", GithubItems::Pulls),
        ("support", GithubItems::Issues),
    ] {
        b.create_source("p", name, Provider::Github, "org/proj", name)
            .unwrap();
        b.patch_source(
            "p",
            name,
            &SourcePatch {
                github_items: Some(kind),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let raw = |is_pr: bool, title: &str| {
        let mut v = json!({"action":"opened","repository":{"full_name":"org/proj"}});
        v[if is_pr { "pull_request" } else { "issue" }] =
            json!({"number":if is_pr{8}else{9},"title":title,"state":"open","user":{"id":2}});
        v
    };
    for name in ["features", "support"] {
        for pr in [true, false] {
            let k = if pr { "pull_request" } else { "issues" };
            ingest(&b, "p", name, k, k, raw(pr, k)).unwrap();
        }
        let r = b.drain_source_report("p", name, 10).unwrap();
        assert_eq!((r.applied, r.skipped, r.failed), (1, 1, 0));
    }
    let pulls = b.find_source("p", "features", "8", None).unwrap().unwrap();
    assert_eq!(pulls.channel, "features");
    assert!(b.find_source("p", "features", "9", None).unwrap().is_none());
    assert_eq!(
        b.find_source("p", "support", "9", None)
            .unwrap()
            .unwrap()
            .channel,
        "support"
    );
    b.patch_source(
        "p",
        "features",
        &SourcePatch {
            github_items: Some(GithubItems::Issues),
            ..Default::default()
        },
    )
    .unwrap();
    ingest(
        &b,
        "p",
        "features",
        "pull_request",
        "amended",
        raw(true, "amended"),
    )
    .unwrap();
    assert_eq!(b.drain_source("p", "features", 10).unwrap(), (1, 0));
    assert!(b.get(pulls.id).unwrap().text.contains("amended"));
    let mock = Mock::new(vec![(
        "GET /repos/org/proj/issues/1 ",
        200,
        json!({"number":1,"pull_request":{}}),
    )]);
    let (db, c) = setup(Provider::Github, "org/proj", &mock);
    db.patch_source(
        "ada",
        "test",
        &SourcePatch {
            github_items: Some(GithubItems::Issues),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(c.pull(&db, "ada", "test", Some("1")).unwrap().records, 0);
    mock.finish();
}

#[test]
fn provider_previews_accept_real_links_contain_readable_rows_and_never_change_a_mapping() {
    let mock = Mock::new(vec![
        (
            "POST /graphql ",
            200,
            json!({"data":{"projects":{"nodes":[{"id":"id-9","name":"Release"}]}}}),
        ),
        (
            "POST /graphql ",
            200,
            json!({"data":{"issues":{"nodes":[{"id":"root","identifier":"ENG-1","title":"Review"}],"pageInfo":{"hasNextPage":true,"endCursor":"more"}}}}),
        ),
        (
            "POST /graphql ",
            200,
            json!({"data":{"issue":{"id":"root","identifier":"ENG-1","title":"Review","team":{"key":"ENG"},"comments":{"nodes":[{"id":"r","body":"second page"}],"pageInfo":{"hasNextPage":false}}}}}),
        ),
    ]);
    let (b, c) = setup(Provider::Linear, "ENG", &mock);
    let before = b.cursor().unwrap();
    let p = c
        .preview_target(
            &b,
            "ada",
            "test",
            Some("https://linear.app/example/project/release-deadbeef"),
            None,
            None,
            2,
        )
        .unwrap();
    assert_eq!(p["records"][0]["title"], "Review");
    assert_eq!(p["next"], "more");
    let q = c
        .preview_target(
            &b,
            "ada",
            "test",
            Some("https://linear.app/example/issue/ENG-1/review"),
            None,
            Some("cursor"),
            3,
        )
        .unwrap();
    assert_eq!(q["records"][0]["change"]["text"], "second page");
    assert_eq!(before, b.cursor().unwrap());
    assert!(
        c.preview(
            &b,
            "ada",
            "test",
            Some("https://example.invalid/issue/ENG-1")
        )
        .is_err()
    );
    let req = mock.finish();
    assert_eq!(
        req[1].1["variables"]["filter"],
        json!({"team":{"key":{"eq":"ENG"}},"project":{"id":{"eq":"id-9"}}})
    );
    assert_eq!(
        req[2].1["variables"],
        json!({"id":"ENG-1","after":"cursor","first":3})
    );
    let s = b
        .create_source("p", "gh", Provider::Github, "me/repo", "dev")
        .unwrap()
        .0;
    assert_eq!(
        github::issue_number(&s, "https://github.com/me/repo/pull/42#discussion_r9").unwrap(),
        42
    );
    assert!(github::issue_number(&s, "https://github.com/another/repo/issues/42").is_err());
}

#[test]
fn cross_provider_tag_filters_keep_display_values_and_sparse_updates_only_own_supplied_fields() {
    let db = Board::memory().unwrap();
    db.create_source("p", "lin", Provider::Linear, "ENG", "dev")
        .unwrap();
    let issue = |action: &str, data: Value| json!({"type":"Issue","action":action,"actor":{"type":"user","name":"agent"},"data":data});
    let mut base = json!({"id":"a123","identifier":"ENG-12","title":"Proposal","description":"ready","team":{"key":"ENG"}});
    let mut opened = base.clone();
    opened["priorityLabel"] = json!("High");
    opened["project"] = json!({"name":"Release Plan"});
    opened["assignee"] = json!({"displayName":"Ana"});
    opened["labels"] = json!([{"name":"._Àrea   V2.1"},{"name":"!!"}]);
    ingest(&db, "p", "lin", "linear", "first", issue("create", opened)).unwrap();
    db.drain_source("p", "lin", 20).unwrap();
    let m = db.find_source("p", "lin", "a123", None).unwrap().unwrap();
    let fields = stored_fields(&db, m.id);
    assert_eq!(fields["tag.area-v2.1"], "._Àrea   V2.1");
    assert_eq!(fields["assignee.ana"], "Ana");
    assert_eq!(fields["project"], "release-plan");
    provider_fields(
        &db,
        "lin",
        "a123",
        &[("tag.extra", "extra"), ("project.notes", "keep")],
    );
    base["assigneeId"] = json!("id-only");
    base["projectId"] = json!("id-only");
    base["priority"] = json!(0);
    ingest(
        &db,
        "p",
        "lin",
        "linear",
        "sparse",
        issue("update", base.clone()),
    )
    .unwrap();
    db.drain_source("p", "lin", 20).unwrap();
    let fields = stored_fields(&db, m.id);
    assert!(
        fields.contains_key("tag.extra")
            && fields.contains_key("assignee.ana")
            && !fields.contains_key("priority")
    );
    assert_eq!(fields["project"], "release-plan");
    base["assigneeId"] = Value::Null;
    base["projectId"] = Value::Null;
    base["labels"] = json!([]);
    ingest(&db, "p", "lin", "linear", "remove", issue("update", base)).unwrap();
    db.drain_source("p", "lin", 20).unwrap();
    let fields = stored_fields(&db, m.id);
    assert!(
        !fields
            .keys()
            .any(|k| k.starts_with("assignee.") || k.starts_with("tag.") || k == "project")
    );
    assert_eq!(fields["project.notes"], "keep");
}

#[test]
fn oversized_native_posts_and_edits_clip_without_splitting_an_astral_character() {
    let text = char::from_u32(0x1f4bb).unwrap().to_string().repeat(6000);
    let mut m = upsert("t", "root".into(), "a".into(), None, text.clone());
    for edit in [false, true] {
        if edit {
            m.change = Change::Edit { text: text.clone() };
        }
        let e = envelope(m.clone(), json!({"original":text}));
        let (Change::Edit { text: body } | Change::Upsert { text: body, .. }) = e.import.change
        else {
            unreachable!()
        };
        assert!(
            body.encode_utf16().count() <= 10000
                && body.ends_with("[truncated; original in connector inbox]")
        );
        assert_eq!(e.raw["original"], text);
    }
}

#[test]
fn a_full_batch_of_skips_does_not_starve_the_next_real_import() {
    let mock = Mock::new(vec![(
        "GET /conversations.replies?",
        200,
        json!({"ok":true,"messages":[{"ts":"10.1","user":"U1","text":"kept"}]}),
    )]);
    let (db, c) = setup(Provider::Slack, "C1", &mock);
    let mut ignored = upsert(
        "other",
        "ignored".into(),
        "owner".into(),
        None,
        String::new(),
    );
    ignored.change = Change::Ignore;
    let batch = (0..500)
        .map(|i| Envelope {
            key: format!("skip-{i}"),
            import: ignored.clone(),
            raw: Value::Null,
        })
        .collect::<Vec<_>>();
    db.enqueue_imports("ada", "test", &batch, None).unwrap();
    let report = c.pull(&db, "ada", "test", Some("10.1")).unwrap();
    assert_eq!((report.skipped, report.applied, report.failed), (500, 1, 0));
    assert_eq!(
        db.find_source("ada", "test", "10.1", None)
            .unwrap()
            .unwrap()
            .text,
        "kept"
    );
    mock.finish();
}

#[test]
fn signed_native_intake_checks_exact_original_bytes_and_the_providers_replay_window() {
    use ring::hmac;
    let secret = b"test signing key";
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    // GitHub's published test vector (independent of the signer used for the other fixtures).
    verify_intake(
        &Provider::Github,
        b"It's a Secret to Everybody",
        b"Hello, World!",
        "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17",
        None,
        1000,
    )
    .unwrap();
    let body = br#"{ "webhookTimestamp":1000000 }"#;
    for (p, prefix, prelude) in [
        (Provider::Github, "sha256=", ""),
        (Provider::Linear, "", ""),
        (Provider::Slack, "v0=", "v0:1000:"),
    ] {
        let bytes = [prelude.as_bytes(), body.as_slice()].concat();
        let sig = format!("{prefix}{}", hex::encode(hmac::sign(&key, &bytes).as_ref()));
        verify_intake(&p, secret, body, &sig, Some("1000"), 1000).unwrap();
        assert!(verify_intake(&p, b"wrong", body, &sig, Some("1000"), 1000).is_err());
        assert!(
            verify_intake(
                &p,
                secret,
                br#"{"webhookTimestamp":1000000}"#,
                &sig,
                Some("1000"),
                1000
            )
            .is_err()
        );
        if p != Provider::Github {
            assert!(verify_intake(&p, secret, body, &sig, Some("1000"), 2000).is_err());
        }
    }
}
#[test]
fn slack_formatting_and_mentions_are_scoped_and_never_ping_from_code_or_raw_control_tags() {
    let (text, profiles) = slack_text::incoming(
        "work",
        "Hi <@U123|Ada>, <https://example.org/x|the link>, *strong*, and `<@U456>`.",
    );
    assert_eq!(
        text,
        "Hi @[source:work/U123], [the link](https://example.org/x), **strong**, and `<@U456>`."
    );
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles["U123"].name, "Ada");
    let sent = slack_text::outgoing(
        "work",
        &format!("{text} <!channel> @[source:somewhere/U789]"),
    );
    assert!(sent.contains("<@U123>"), "{sent}");
    assert!(sent.contains("<https://example.org/x|the link>") && sent.contains("*strong*"));
    assert!(!sent.contains("<!channel>") && !sent.contains("<@U456>") && !sent.contains("<@U789>"));
}
#[test]
fn automatic_slack_files_profiles_attribution_status_and_file_removal_use_confirmed_copies() {
    let user = |id: &str, name: &str| json!({"ok":true,"user":{"id":id,"profile":{"display_name":name,"image_72":"https://example.invalid/avatar.png"}}});
    let mock = Mock::new(vec![
        (
            "GET /conversations.replies?",
            200,
            json!({"ok":true,"messages":[{"ts":"10.1","user":"U1","text":"Hello <@U2>","files":[{"id":"F1","name":"note.json"}]}]}),
        ),
        ("GET /users.info?", 200, user("U1", "Reporter")),
        ("GET /users.info?", 200, user("U2", "Reader")),
        (
            "GET /files.info?",
            200,
            json!({"ok":true,"file":{"id":"F1","name":"note.json","size":2,"url_private_download":"https://files.slack.com/data","shares":{"private":{"C1":[{"ts":"10.1"}]}}}}),
        ),
        ("GET /data ", 200, json!({})),
        (
            "POST /chat.postMessage ",
            200,
            json!({"ok":true,"ts":"11.2"}),
        ),
        (
            "POST /files.getUploadURLExternal?",
            200,
            json!({"ok":true,"file_id":"F2","upload_url":"https://files.slack.com/upload-raw"}),
        ),
        ("POST /upload-raw ", 200, json!({})),
        (
            "POST /files.completeUploadExternal ",
            200,
            json!({"ok":true,"files":[{"id":"F2"}]}),
        ),
        (
            "POST /assistant.threads.setStatus ",
            200,
            json!({"ok":true}),
        ),
        ("POST /files.delete ", 200, json!({"ok":true})),
        ("POST /chat.delete ", 200, json!({"ok":true})),
    ]);
    let (db, c) = setup(Provider::Slack, "C1", &mock);
    db.patch_source(
        "ada",
        "test",
        &SourcePatch {
            outbound: Some(true),
            mirror_files: Some(true),
            display_authors: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    let r = c.pull(&db, "ada", "test", Some("10.1")).unwrap();
    assert_eq!((r.files, r.files_deferred), (1, 0));
    let m = db
        .find_source("ada", "test", "10.1", None)
        .unwrap()
        .unwrap();
    assert_eq!(m.text, "Hello @[source:test/U2]");
    assert_eq!(m.author.unwrap().name, "Reporter");
    assert_eq!(m.attachments.len(), 1);
    let mine = db
        .reply(
            m.id,
            "ada",
            "Hello **team**",
            &PostOptions {
                files: vec![("proof".into(), br#"{}"#.to_vec())],
                ..Default::default()
            },
        )
        .unwrap();
    for _ in 0..2 {
        assert!(c.send_next(&db, "ada", "test").unwrap().is_some());
    }
    assert!(c.send_next(&db, "ada", "test").unwrap().is_none());
    c.slack_status(&db, "ada", "test", "10.1", "checking")
        .unwrap();
    db.delete(mine.id, "ada", None).unwrap();
    for _ in 0..2 {
        assert!(c.send_next(&db, "ada", "test").unwrap().is_some());
    }
    assert!(db.source_files("ada", "test", mine.id).unwrap().is_empty());
    let req = mock.finish();
    assert_eq!(req[5].1["text"], "Hello *team*\n\nFrom ada on Board");
    assert_eq!(req[10].1["file"], "F2");
}
#[test]
fn native_pr_lifecycle_preserves_unsupplied_tags_and_clears_only_the_old_heads_ci() {
    let db = Board::memory().unwrap();
    db.create_source("p", "gh", Provider::Github, "a/b", "dev")
        .unwrap();
    let raw = |act: &str, head: &str| json!({"repository":{"id":3,"full_name":"a/b"},"action":act,"sender":{"id":7,"login":"ada"},"pull_request":{"number":4,"title":"PR","state":if act=="closed"{"closed"}else{"open"},"head":{"sha":head}}});
    let mut opened = raw("opened", "old");
    opened["pull_request"]["labels"] = json!([{"name":"needs-test"}]);
    ingest(&db, "p", "gh", "pull_request", "open", opened).unwrap();
    db.drain_source("p", "gh", 5).unwrap();
    let m = db.find_source("p", "gh", "4", None).unwrap().unwrap();
    provider_fields(
        &db,
        "gh",
        "4",
        &[("ci.3-4.build", "failure"), ("ci.other", "keep")],
    );
    ingest(
        &db,
        "p",
        "gh",
        "pull_request",
        "head",
        raw("synchronize", "new"),
    )
    .unwrap();
    db.drain_source("p", "gh", 5).unwrap();
    for _ in 0..2 {
        ingest(&db, "p", "gh", "pull_request", "end", raw("closed", "new")).unwrap();
        db.drain_source("p", "gh", 5).unwrap();
    }
    let t = db.conversation(m.id, &Default::default()).unwrap();
    let fields = stored_fields(&db, m.id);
    assert_eq!(fields["head"], "new");
    assert!(fields.contains_key("tag.needs-test") && !fields.contains_key("ci.3-4.build"));
    assert_eq!(fields["ci.other"], "keep");
    assert_eq!(t.replies.len(), 1);
    assert_eq!(t.replies[0].text, "closed by ada");
}
#[test]
fn native_github_child_can_seed_its_root_offline_but_ci_and_push_alone_never_do() {
    let b = Board::memory().unwrap();
    b.create_source("p", "gh", Provider::Github, "org/repo", "work")
        .unwrap();
    assert!(
        b.patch_source(
            "p",
            "gh",
            &SourcePatch {
                mirror_roots: Some(true),
                ..Default::default()
            }
        )
        .is_err()
    );
    let pr = json!({"id":12,"number":3,"title":"A change","body":"Context","state":"open","head":{"sha":"a"},"user":{"id":7,"login":"ada"}});
    let repo = json!({"id":9,"full_name":"org/repo"});
    ingest(
        &b,
        "p",
        "gh",
        "pull_request",
        "update",
        json!({"action":"synchronize","repository":repo,"pull_request":pr}),
    )
    .unwrap();
    let run = json!({"action":"completed","repository":repo,"workflow_run":{"id":55,"head_sha":"a","conclusion":"cancelled","pull_requests":[{"number":3}]}});
    ingest(&b, "p", "gh", "workflow_run", "cancelled", run).unwrap();
    assert_eq!(b.drain_source("p", "gh", 20).unwrap(), (0, 0));
    assert!(b.messages(&Default::default(), 5).unwrap().is_empty());
    let c = json!({"action":"created","repository":repo,"pull_request":pr,"comment":{"id":4,"body":"inline","path":"x.rs","line":3,"user":{"id":8,"login":"reviewer"}}});
    for _ in 0..2 {
        ingest(
            &b,
            "p",
            "gh",
            "pull_request_review_comment",
            "early",
            c.clone(),
        )
        .unwrap();
        b.drain_source("p", "gh", 20).unwrap();
    }
    let m = b.find_source("p", "gh", "3", None).unwrap().unwrap();
    let t = b.conversation(m.id, &Default::default()).unwrap();
    assert!(m.text.contains("A change"));
    assert_eq!(t.replies.len(), 1);
    assert!(t.replies[0].text.contains("inline"));
}
#[test]
fn a_private_or_shared_slack_target_is_never_previewed_or_joined_and_paging_is_read_only() {
    let channel = |public: bool| json!({"ok":true,"channel":{"id":"C2","is_channel":true,"is_shared":false,"is_private":!public,"is_member":true}});
    let mock = Mock::new(vec![
        ("GET /conversations.info?", 200, channel(false)),
        ("GET /conversations.info?", 200, channel(true)),
        (
            "GET /conversations.history?",
            200,
            json!({"ok":true,"messages":[{"type":"message","ts":"2.1"}],"response_metadata":{"next_cursor":"more"}}),
        ),
    ]);
    let (db, c) = setup(Provider::Slack, "C1", &mock);
    let cursor = db.cursor().unwrap();
    assert!(c.join_slack_target(&db, "ada", "test", Some("C2")).is_err());
    let page = c
        .preview_target(&db, "ada", "test", None, Some("C2"), Some("prior"), 3)
        .unwrap();
    assert_eq!(page["threads"], json!(["2.1"]));
    assert_eq!(page["records"][0]["external"], "2.1");
    assert_eq!(page["next"], "more");
    assert_eq!(db.cursor().unwrap(), cursor);
    let req = mock.finish();
    assert!(req[2].0.contains("cursor=prior") && req[2].0.contains("limit=3"));
}
#[test]
fn slack_on_demand_rechecks_membership_and_resolves_public_names_and_reply_timestamps() {
    let channel = |member: bool| json!({"ok":true,"channel":{"id":"C1","is_channel":true,"is_shared":false,"is_private":false,"is_member":member}});
    let mock = Mock::new(vec![
        ("GET /conversations.info?", 200, channel(false)),
        (
            "GET /conversations.list?",
            200,
            json!({"ok":true,"channels":[],"response_metadata":{"next_cursor":"p2"}}),
        ),
        (
            "GET /conversations.list?",
            200,
            json!({"ok":true,"channels":[{"name":"eng","id":"C1"}]}),
        ),
        ("GET /conversations.info?", 200, channel(true)),
        (
            "GET /conversations.replies?",
            200,
            json!({"ok":true,"messages":[{"ts":"2.1","thread_ts":"1.1"}]}),
        ),
        (
            "GET /conversations.replies?",
            200,
            json!({"ok":true,"messages":[{"ts":"1.1","text":"topic"},{"ts":"2.1","text":"reply","thread_ts":"1.1"}],"response_metadata":{"next_cursor":"p3"}}),
        ),
    ]);
    let (b, c) = setup(Provider::Slack, "C1", &mock);
    let old = b.cursor().unwrap();
    assert!(c.preview(&b, "ada", "test", None).is_err());
    let out = c.preview(&b, "ada", "test", Some("#eng:2.1")).unwrap();
    assert_eq!(out["thread"], "1.1");
    assert_eq!(out["next"], "p3");
    assert_eq!(out["records"][1]["change"]["text"], "reply");
    assert_eq!(old, b.cursor().unwrap());
    let rows = mock.finish();
    assert!(rows[2].0.contains("cursor=p2") && rows[5].0.contains("ts=1.1"));
}
#[test]
fn refused_file_becomes_a_persistent_text_note_and_unavailable_inbound_bytes_do_not_block_the_thread()
 {
    let page = json!({"ok":true,"messages":[{"ts":"1.1","user":"U1","text":"root","files":[{"id":"FX","name":"large.mov","size":30000000}]}]});
    let mock = Mock::new(vec![
        ("GET /conversations.replies?", 200, page),
        (
            "POST /chat.postMessage ",
            200,
            json!({"ok":true,"ts":"2.1"}),
        ),
        (
            "POST /files.getUploadURLExternal?",
            200,
            json!({"ok":false,"error":"missing_scope"}),
        ),
        ("POST /chat.update ", 200, json!({"ok":true})),
        ("POST /chat.update ", 200, json!({"ok":true})),
    ]);
    let (b, c) = setup(Provider::Slack, "C1", &mock);
    b.patch_source(
        "ada",
        "test",
        &SourcePatch {
            mirror_files: Some(true),
            outbound: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    c.pull(&b, "ada", "test", Some("1.1")).unwrap();
    let r = b.find_source("ada", "test", "1.1", None).unwrap().unwrap();
    assert_eq!(r.file_notices[0].name, "large.mov");
    assert_eq!(r.file_notices[0].size, Some(30000000));
    let m = b
        .reply(
            r.id,
            "ada",
            "result",
            &PostOptions {
                files: vec![("run.log".into(), b"{}".to_vec())],
                ..Default::default()
            },
        )
        .unwrap();
    for state in ["done", "skipped", "done"] {
        assert_eq!(
            c.send_next(&b, "ada", "test").unwrap().unwrap().state,
            state
        );
    }
    b.edit(m.id, "ada", "revised", None).unwrap();
    c.send_next(&b, "ada", "test").unwrap();
    let req = mock.finish();
    assert!(req[3].1["text"].as_str().unwrap().contains("run.log"));
    assert!(
        req[4].1["text"].as_str().unwrap().starts_with("revised")
            && req[4].1["text"].as_str().unwrap().contains("run.log")
    );
}
#[test]
fn slack_unfurls_forwarded_messages_and_never_reads_a_private_link_with_bot_credentials() {
    let private = "https://acme.slack.com/archives/CSECRET/p1234567890000000";
    let public = "https://acme.slack.com/archives/CPUBLIC/p1234567891000000";
    let page = json!({"ok":true,"messages":[{"ts":"2.1","user":"U1","text":format!("{private} {public}")}]});
    let mock = Mock::new(vec![
        ("GET /conversations.replies?", 200, page),
        (
            "GET /conversations.info?",
            200,
            json!({"ok":true,"channel":{"id":"CSECRET","is_channel":true,"is_private":true,"is_member":true}}),
        ),
        (
            "GET /conversations.info?",
            200,
            json!({"ok":true,"channel":{"id":"CPUBLIC","is_channel":true,"is_private":false,"is_shared":false,"is_member":true}}),
        ),
        (
            "GET /conversations.replies?",
            200,
            json!({"ok":true,"messages":[{"ts":"1234567891.000000","user":"U8","text":"reviewed"}]}),
        ),
    ]);
    let (b, c) = setup(Provider::Slack, "C1", &mock);
    c.pull(&b, "ada", "test", Some("2.1")).unwrap();
    let r = b.find_source("ada", "test", "2.1", None).unwrap().unwrap();
    assert_eq!(r.links.len(), 2);
    assert_eq!(r.links[0].reason.as_deref(), Some("not_public"));
    assert_eq!(r.links[1].text.as_deref(), Some("reviewed"));
    let f = json!({"event":{"type":"message","user":"U3","channel":"C1","ts":"3.1","attachments":[{"is_share":true,"from_url":public,"text":"forwarded","author_name":"Ada"}]}});
    ingest(&b, "ada", "test", "slack", "forward", f).unwrap();
    b.drain_source("ada", "test", 3).unwrap();
    let r = b.find_source("ada", "test", "3.1", None).unwrap().unwrap();
    assert!(r.text.contains("forwarded"));
    assert_eq!(r.links[0].author.as_deref(), Some("Ada"));
    assert_eq!(mock.finish().len(), 4);
}
