use super::*;
use board_local::swarm::{SpawnedWorker, SwarmAddWorker, SwarmRun, SwarmStart};

#[derive(Clone, Default)]
struct Steps(Vec<Value>);
impl Steps {
    fn push(&mut self, mut e: Value) {
        e["t"] = json!((self.0.len() + 1) as f64 * 0.2);
        self.0.push(e);
    }
    fn tap(&mut self, key: impl Into<String>) {
        self.push(json!({"type":"tap","key":key.into()}));
    }
    fn paste(&mut self, v: &str) {
        self.push(json!({"paste":v}));
    }
    fn replace(&mut self, key: &str, v: &str) {
        self.tap(key);
        self.push(json!({"type":"key","name":"a","cmd":true}));
        self.paste(v);
    }
    fn end(&self) -> String {
        format!("{:.1}", self.0.len() as f64 * 0.2 + 0.7)
    }
    fn view(&self, f: &Fixture) -> Tree {
        f.render(json!(&self.0), "1200x900", &self.end())
    }
    fn swarm(id: i64) -> Self {
        let mut s = Self::default();
        s.tap("swarms");
        s.tap(format!("swarm-run-{id}"));
        s
    }
    fn library() -> Self {
        let mut s = Self::default();
        s.tap("workspace");
        s.tap("work-roles");
        s
    }
}
fn start(b: &Board) -> SwarmRun {
    b.swarm_start(
        "reader",
        &SwarmStart {
            goal: "Verify offline restores".into(),
            slug: Some("restore-audit".into()),
            ..Default::default()
        },
    )
    .unwrap()
}
fn add(b: &Board, run: &SwarmRun, name: &str, role: &str) -> SpawnedWorker {
    b.swarm_add_worker(
        "reader",
        &run.slug,
        &SwarmAddWorker {
            name: name.into(),
            role_slug: Some(role.into()),
            brief: format!("Own {name} checks for restore"),
            branch_name: Some(format!("test/{name}")),
            workspace_path: Some(format!("worktrees/{name}")),
        },
    )
    .unwrap()
}
fn seed(b: &Board) -> SwarmRun {
    let r = start(b);
    add(b, &r, "impl", "implementer");
    add(b, &r, "verify", "verifier");
    r
}
fn has(t: &Tree, needle: &str) -> bool {
    t.0.iter()
        .any(|e| e["text"].as_str().is_some_and(|v| v.contains(needle)))
}

#[test]
fn evidence_cards_preserve_custom_schemas_from_before_v15() {
    let f = Fixture::new();
    let historical =
        include_bytes!("../../../crates/board-core/tests/fixtures/swarm-v14.sqlite3.gz");
    std::io::copy(
        &mut flate2::read::GzDecoder::new(&historical[..]),
        &mut File::create(&f.db).unwrap(),
    )
    .unwrap();
    let old = rusqlite::Connection::open(&f.db).unwrap();
    for (name, field) in [("verdict", "decision"), ("diff", "delta")] {
        let definition = json!({"type":"object", "required":[field]});
        old.execute(
            "INSERT INTO schemas(name,version,description,definition) VALUES(?1,1,'Custom schema',?2)",
            (name, definition.to_string()),
        )
        .unwrap();
        old.execute(
            "INSERT INTO schema_origins(name,version,builtin,actor) VALUES(?,1,0,'reader')",
            [name],
        )
        .unwrap();
    }
    drop(old);
    let b = Board::open(&f.db).unwrap();
    let run = start(&b);
    let content = concat!(
        "```json#verdict@1\n{\"decision\":\"Awaiting approval\"}\n```\n\n",
        "```json#diff@1\n{\"delta\":\"Four records\",\"patch\":\"revision 3\"}\n```"
    );
    b.swarm_post("reader", &run.slug, content, &Default::default())
        .unwrap();
    let t = Steps::swarm(run.id).view(&f);
    for label in ["decision", "Awaiting approval", "delta", "Four records"] {
        t.expect_text(label);
    }
    assert!(!has(&t, "NOT RUN"));
}

#[test]
fn observer_paints_public_evidence_and_dms_without_waking_workers() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let run = seed(&b);
    b.swarm_set_worker_status(
        "reader/verify",
        &run.slug,
        "verify",
        "waiting",
        Some(std::process::id()),
    )
    .unwrap();
    let content = concat!(
        "@verify ready\n\n",
        "```json#verdict@1\n{\"status\":\"pass\",\"artifact\":\"cycle-2\",\"summary\":\"Restore verified\",\"checks\":[{\"name\":\"Cold run\",\"status\":\"pass\",\"command\":\"cargo test restore\",\"output\":\"one test passed\"}]}\n```\n\n",
        "```json#diff@1\n{\"base\":\"abc\",\"head\":\"def\",\"files\":[\"src/restore.rs\"],\"patch\":\"-old path\\n+new path\"}\n```"
    );
    let m = b
        .swarm_post("reader/impl", &run.slug, content, &Default::default())
        .unwrap()
        .message;
    let dm = b
        .swarm_dm(
            "reader/verify",
            &run.slug,
            "impl",
            "A private peer exchange",
        )
        .unwrap();
    let cursor = b.cursor().unwrap();
    let before = b.swarm_status("reader", &run.slug).unwrap();
    let mut steps = Steps::swarm(run.id);
    steps.tap("swarm-brief-impl");
    for theme in ["dark", "light"] {
        let (t, frame) = f.snapshot(
            json!(steps.0),
            (1200, 900),
            theme,
            &format!("swarm-restores-{theme}"),
        );
        let roster = t.bounds("swarm-roster-scroll");
        let stream = t.bounds("swarm-feed");
        assert!(
            roster.w >= 180. && roster.edge() <= stream.x + 2.,
            "roster and feed overlap: {roster:?} {stream:?}"
        );
        for text in [
            "Verify offline restores",
            "Own impl checks for restore",
            "Branch: test/impl",
            "registered",
            "waiting",
            "implementer",
            "verifier",
            "Brief for ",
            "@verify",
            "Notified @verify",
            "A private peer exchange",
            "PASS",
            "Restore verified",
            "abc..def",
            "src/restore.rs",
            "+new path",
            "-old path",
        ] {
            assert!(has(&t, text), "missing {text}");
        }
        assert!(
            !has(&t, "@[reader/verify]") && !has(&t, "@[reader/impl]"),
            "resolved mention should render cleanly without raw @[actor] markup"
        );
        for (x, kind) in [(0, "deletion"), (1, "addition")] {
            let row = t.bounds(&format!("swarm-evidence-{}-1-line-{x}", m.id));
            assert!(row.y > 0. && row.bottom() < 900., "{row:?}");
            let [r, g, _, _] = frame[((row.edge() - 4.) as u32, (row.y + row.h * 0.5) as u32)].0;
            assert!(
                if x == 0 { r > g + 6 } else { g > r + 6 },
                "{theme} {kind} line must paint its actual diff tint: {r}/{g}"
            );
        }
    }
    let mut checks = steps.clone();
    checks.tap(format!("swarm-evidence-{}-0-check-0", m.id));
    let t = checks.view(&f);
    t.expect_text("cargo test restore");
    t.expect_text("one test passed");
    let mut raw = steps.clone();
    raw.tap(format!("swarm-raw-{}-1", m.id));
    assert!(has(&raw.view(&f), "\"patch\":"));
    let mut filtered = steps;
    filtered.tap("swarm-kind-dm");
    let t = filtered.view(&f);
    t.expect_key(&format!("swarm-entry-{}", dm.event_cursor));
    assert!(!has(&t, "Restore verified"));
    let after = b.swarm_status("reader", &run.slug).unwrap();
    assert_eq!(
        (after.notifications_sent, b.cursor().unwrap()),
        (before.notifications_sent, cursor)
    );
    assert_eq!(
        after
            .workers
            .iter()
            .map(|w| (&w.updated_at, w.waiter_pid))
            .collect::<Vec<_>>(),
        before
            .workers
            .iter()
            .map(|w| (&w.updated_at, w.waiter_pid))
            .collect::<Vec<_>>()
    );
}

#[test]
fn human_posts_and_direct_messages_have_distinct_routing_and_stopping_requires_confirmation() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let run = seed(&b);
    let lead = Steps::swarm(run.id);
    let mut s = lead.clone();
    s.tap("swarm-body");
    s.paste("Public progress update");
    s.tap("swarm-send");
    s.tap("swarm-target-trigger");
    s.tap("option2");
    s.paste("Direct note for implementation");
    s.tap("swarm-send");
    let t = s.view(&f);
    t.expect_text("Direct note for implementation");
    let history = b.swarm_read("reader", &run.slug, 0, 32, true).unwrap();
    assert!(history.entries.iter().any(|e| {
        e.message
            .as_ref()
            .is_some_and(|m| m.text == "Public progress update")
            && e.notified.is_empty()
    }));
    assert!(
        history
            .entries
            .iter()
            .any(|e| e.dm.as_ref().is_some_and(
                |dm| dm.text == "Direct note for implementation" && dm.to_worker == "impl"
            ))
    );
    let mut stopping = lead;
    stopping.tap("swarm-stop");
    stopping.view(&f).expect_key("swarm-stop-confirm");
    assert_eq!(
        b.swarm_status("reader", &run.slug).unwrap().status,
        "active"
    );
    stopping.tap("swarm-stop-confirm");
    let t = stopping.view(&f);
    assert!(!t.has_key("swarm-composer"));
    assert_eq!(
        b.swarm_status("reader", &run.slug).unwrap().status,
        "stopped"
    );
}

#[test]
fn open_observer_sees_an_external_peer_exchange_and_hides_unrelated_dms_from_a_scoped_profile() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let run = seed(&b);
    let mut s = Steps::swarm(run.id);
    s.push(json!({"type":"mark","name":"observer"}));
    let mut cmd = f.command(json!(s.0), "1100x760", "2.8");
    let live = f.at_mark(&mut cmd, "observer", || {
        b.swarm_dm("reader", &run.slug, "verify", "Restricted exchange")
            .unwrap();
        b.swarm_set_worker_status(
            "reader/impl",
            &run.slug,
            "impl",
            "waiting",
            Some(std::process::id()),
        )
        .unwrap();
    });
    live.expect_text("Restricted exchange");
    live.expect_text("waiting");
    let actor = "reader/impl";
    let grant = b
        .set_grant(
            "reader",
            None,
            actor,
            &board_local::access::Rules::parse("swarms/restore-audit=read,post").unwrap(),
        )
        .unwrap();
    let token = b
        .issue_credential("reader", actor, None, None)
        .unwrap()
        .token;
    b.swarm_dm("reader", &run.slug, "impl", "Own exchange")
        .unwrap();
    let route = Steps::swarm(run.id);
    let run_ui = |steps: &Steps| {
        let o = f
            .command(json!(steps.0), DESKTOP, &steps.end())
            .env("BOARD_TOKEN", &token)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        f.tree()
    };
    let t = run_ui(&route);
    t.expect_text("Own exchange");
    assert!(!has(&t, "Restricted exchange"));
    assert!(!t.has_key("swarm-stop"));
    let roles = run_ui(&Steps::library());
    assert!(!roles.has_key("role-implementer"));
    assert!(has(&roles, "local board owner"));
    let mut cmd = f.command(json!(s.0), DESKTOP, "2.8");
    cmd.env("BOARD_TOKEN", &token);
    let t = f.at_mark(&mut cmd, "observer", || {
        b.set_grant(
            "reader",
            Some(grant.id),
            actor,
            &board_local::access::Rules::parse("elsewhere=read").unwrap(),
        )
        .unwrap();
    });
    assert!(
        !has(&t, "Own exchange") && !has(&t, "Own impl checks") && !t.has_key("swarm-composer")
    );
}

#[test]
fn role_settings_attribute_an_edit_to_the_profile_selected_in_rune() {
    let f = Fixture::new();
    let mut steps = Steps::default();
    steps.tap("workspace");
    steps.tap("work-profiles");
    steps.replace("profile-name", "curator");
    steps.tap("switch-profile");
    steps.tap("workspace");
    steps.tap("work-roles");
    steps.tap("role-implementer");
    steps.replace("role-prompt", "Work as the selected profile.");
    steps.tap("role-save");
    steps.view(&f);
    let b = Board::open(&f.db).unwrap();
    let (_, events) = b
        .event_page(0, 10, &Default::default(), Some("role_library.changed"))
        .unwrap();
    let actors: Vec<_> = events.iter().map(|e| e.actor.as_str()).collect();
    assert_eq!(actors, ["reader/curator"]);
}

#[test]
fn role_prompt_restore_skill_import_attachment_and_guarded_library_delete_use_sqlite() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let run = start(&b);
    let builtin = b.role_show("implementer").unwrap();
    let mut roles = Steps::library();
    roles.tap("role-implementer");
    let mut edit = roles.clone();
    edit.replace("role-prompt", "Pinned backup procedure");
    edit.tap("role-save");
    edit.view(&f);
    let changed = b.role_show("implementer").unwrap();
    assert_eq!(changed.system_prompt, "Pinned backup procedure");
    assert!(changed.version > builtin.version);
    let pinned = add(&b, &run, "impl", "implementer");
    assert!(pinned.assembled_prompt.contains("Pinned backup procedure"));
    let mut reset = roles.clone();
    reset.tap("role-reset");
    reset.view(&f).expect_key("role-confirmation");
    reset.tap("role-confirm");
    reset.view(&f);
    assert_eq!(
        b.role_show("implementer").unwrap().system_prompt,
        builtin.system_prompt
    );
    let skill = f.dir.path().join("restore-proof");
    fs::create_dir(&skill).unwrap();
    fs::write(skill.join("SKILL.md"),"---\nname: restore-proof\ndescription: Restore evidence\n---\nCheck the disposable database.").unwrap();
    let mut import = roles.clone();
    import.tap("role-import-folder");
    import.tap("skill-browse-path");
    import.push(json!({"type":"file-dialog","path":skill}));
    import.tap("skill-import-save");
    import.view(&f);
    assert_eq!(
        b.skill_show("restore-proof").unwrap().info.roles,
        ["implementer"]
    );
    let mut attach = Steps::library();
    attach.tap("role-verifier");
    attach.tap("role-attach-skill");
    attach.tap("role-attach-restore-proof");
    attach.view(&f);
    assert_eq!(
        b.skill_show("restore-proof").unwrap().info.roles,
        ["implementer", "verifier"]
    );
    let mut library = Steps::library();
    library.tap("skills-tab");
    library.tap("skill-restore-proof");
    library.tap("skill-delete");
    let warning = library.view(&f);
    assert!(has(&warning, "implementer, verifier"));
    warning.expect_key("role-confirmation");
    let mut detach = roles;
    detach.tap("role-detach-restore-proof");
    detach.view(&f);
    assert_eq!(
        b.skill_show("restore-proof").unwrap().info.roles,
        ["verifier"]
    );
    library.tap("role-confirm");
    library.view(&f);
    assert!(b.skill_show("restore-proof").is_err());
    assert!(
        !b.role_show("verifier")
            .unwrap()
            .skills
            .contains(&"restore-proof".into())
    );
    let t = Steps::swarm(run.id).view(&f);
    t.expect_key("swarm-member-impl");
    assert_eq!(
        b.swarm_status("reader", &run.slug).unwrap().workers[0].pinned_role_version,
        Some(changed.version)
    );
}

#[test]
fn worker_filters_search_older_history_and_compact_roster_returns_to_activity() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let run = seed(&b);
    b.swarm_dm("reader", &run.slug, "verify", "Old addressed handoff")
        .unwrap();
    b.swarm_dm("reader", &run.slug, "impl", "Other private note")
        .unwrap();
    for n in 1..=32 {
        b.swarm_post(
            "reader",
            &run.slug,
            &format!("Later public post {n}"),
            &Default::default(),
        )
        .unwrap();
    }
    let mut s = Steps::default();
    s.tap("open-nav");
    s.tap("swarms");
    s.tap(format!("swarm-run-{}", run.id));
    s.tap("swarm-roster-tab");
    s.tap("swarm-brief-verify");
    let t = f.render(json!(s.0), "390x760", &s.end());
    assert!(has(&t, "Own verify checks for restore"));
    let roster = t.bounds("swarm-roster-scroll");
    assert!(roster.w > 260. && roster.edge() <= 390.);
    s.tap("swarm-filter-verify");
    s.tap("swarm-kind-dm");
    let t = f.render(json!(s.0), "390x760", &s.end());
    t.expect_text("Old addressed handoff");
    assert!(!has(&t, "Other private note") && !has(&t, "Later public post"));
    t.expect_key("swarm-clear-worker");
}

#[cfg(unix)]
#[test]
fn a_stale_waiter_is_marked_offline_without_writing_agent_status_or_advancing_the_log() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let run = seed(&b);
    let pid = i32::MAX as u32;
    b.swarm_set_worker_status("reader/verify", &run.slug, "verify", "waiting", Some(pid))
        .unwrap();
    let cursor = b.cursor().unwrap();
    let s = Steps::swarm(run.id);
    let out = f
        .command(json!(s.0), DESKTOP, "6.7")
        .arg("--realtime")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    f.tree().expect_text("Offline waiter");
    assert_eq!(b.cursor().unwrap(), cursor);
    let run = b.swarm_status("reader", &run.slug).unwrap();
    assert!(
        run.workers
            .iter()
            .any(|w| w.name == "verify" && w.status == "waiting" && w.waiter_pid == Some(pid))
    );
}

#[test]
fn custom_role_changes_reject_a_stale_editor_and_confirm_destructive_actions() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let mut s = Steps::library();
    s.tap("role-new");
    s.replace("role-slug", "auditor");
    s.replace("role-name", "Auditor");
    s.replace("role-prompt", "Review backups only");
    s.tap("role-save");
    s.view(&f);
    let saved = b.role_show("auditor").unwrap();
    let mut update = Steps::library();
    update.tap("role-auditor");
    update.replace("role-prompt", "My uncommitted text");
    update.push(json!({"type":"mark","name":"editing"}));
    update.push(json!({"t":2.8,"type":"tap","key":"role-save"}));
    update.0.last_mut().unwrap()["t"] = json!(2.8);
    let mut cmd = f.command(json!(update.0), DESKTOP, "3.2");
    let tree = f.at_mark(&mut cmd, "editing", || {
        b.role_edit(
            &saved.slug,
            &saved.slug,
            &saved.name,
            "",
            "External edit",
            Some(saved.version),
        )
        .unwrap();
    });
    assert!(has(&tree, "Role changed outside this editor"));
    tree.expect_key("role-reload");
    assert_eq!(
        b.role_show("auditor").unwrap().system_prompt,
        "External edit"
    );
    let mut del = Steps::library();
    del.tap("role-auditor");
    del.tap("role-delete");
    del.view(&f).expect_key("role-confirmation");
    assert!(b.role_show("auditor").is_ok());
    del.tap("role-confirm");
    del.view(&f);
    assert!(b.role_show("auditor").is_err());
}
