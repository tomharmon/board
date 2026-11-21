use super::*;
use board_local::{
    access::Rules,
    sources::{GithubItems, Provider, SourcePatch},
};

struct Steps(Vec<Value>);
impl Steps {
    fn sync() -> Self {
        let mut s = Self(Vec::new());
        s.tap("workspace");
        s.tap("work-sources");
        s
    }
    fn step(&mut self, mut action: Value) {
        action["t"] = json!((self.0.len() + 1) as f64 * 0.2);
        self.0.push(action);
    }
    fn tap(&mut self, key: &str) {
        self.step(json!({"type":"tap","key":key}));
    }
    fn input(&mut self, key: &str, text: &str) {
        self.tap(key);
        self.step(json!({"paste":text}));
    }
    fn render(&self, f: &Fixture) -> Tree {
        f.render(json!(self.0), DESKTOP, &self.end())
    }
    fn end(&self) -> String {
        format!("{:.1}", (self.0.len() + 2) as f64 * 0.2)
    }
    fn create(&mut self, provider: &str, name: &str, remote: &str, channel: &str) {
        self.tap("source-add");
        self.tap(&format!("source-provider-{provider}"));
        self.input("source-name", name);
        self.input("source-remote", remote);
        self.input("source-channel", channel);
        self.tap("source-create");
    }
}

#[test]
fn the_source_setup_form_fits_a_phone_and_is_keyboard_reachable() {
    let f = Fixture::new();
    let (tree, pixels) = f.snapshot(
        json!([
            {"t":0.1,"type":"tap","key":"open-nav"},
            {"t":0.3,"type":"tap","key":"workspace"},
            {"t":0.5,"type":"tap","key":"work-sources"},
            {"t":0.7,"type":"tap","key":"source-add"},
            {"t":0.9,"paste":"keyboard-name"}
        ]),
        (390, 844),
        "light",
        "phone-source-setup",
    );
    tree.expect_text("keyboard-name");
    for key in [
        "source-provider-local",
        "source-provider-linear",
        "source-name",
        "source-remote",
        "source-channel",
        "source-create",
        "source-cancel",
    ] {
        let r = tree.bounds(key);
        assert!(
            r.w > 0.
                && r.h > 0.
                && r.x >= 0.
                && r.edge() <= 391.
                && r.y >= 0.
                && r.bottom() <= 845.,
            "{key} outside the screen: {r:?}"
        );
    }
    assert_eq!(pixels.dimensions(), (390, 844));
}

#[cfg(unix)]
#[test]
fn an_inspect_launcher_non_utf8_board_path_opens_that_board_without_offering_a_command() {
    use std::os::unix::ffi::OsStrExt;
    let f = Fixture::new();
    let path = f
        .dir
        .path()
        .join(std::ffi::OsStr::from_bytes(b"board-\xff.db"));
    let db = Board::open(&path).unwrap();
    db.create_source("reader", "local", Provider::Local, "stream", "work")
        .unwrap();
    let mut steps = Steps::sync();
    steps.tap("source-local");
    let fallback = f.dir.path().join("xdg");
    let out = f
        .command(json!(steps.0), DESKTOP, &steps.end())
        .env("BOARD_DB", &path)
        .env("XDG_DATA_HOME", &fallback)
        .env_remove("BOARD_NAME")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let tree = f.tree();
    tree.expect_key("source-configure");
    tree.expect_text("Command unavailable for this database path.");
    assert!(!tree.has_key("source-copy-command"));
    assert!(!fallback.join("board/board.sqlite3").exists());
}

#[test]
fn empty_board_sync_settings_create_all_four_sources_and_opt_in_only_after_confirmation() {
    let f = Fixture::new();
    assert!(!f.db.exists());
    let empty = Steps::sync().render(&f);
    empty.expect_key("source-add");
    empty.expect_text("No sources configured for this profile.");
    for (provider, name, remote, kind) in [
        ("github", "bugs", "sample/repo", Provider::Github),
        ("slack", "chat", "C01234", Provider::Slack),
        ("linear", "tickets", "ENG", Provider::Linear),
        ("local", "stream", "events", Provider::Local),
    ] {
        let mut steps = Steps::sync();
        steps.create(provider, name, remote, "work/review");
        let tree = steps.render(&f);
        tree.expect_key("source-configure");
        tree.expect_key("source-copy-command");
        tree.expect_text("Publishing off");
        let command = if kind == Provider::Local {
            format!("board sync import {name} event.json")
        } else {
            format!("board sync pull {name}")
        };
        tree.expect_text(&command);
        let db = Board::open(&f.db).unwrap();
        let cfg = db.source("reader", name).unwrap();
        assert_eq!(cfg.provider, kind);
        assert_eq!(cfg.remote, remote);
        assert_eq!(cfg.channel, "work/review");
        assert!(!cfg.outbound && !cfg.settings.mirror_roots && !cfg.settings.mirror_files);
        if let Some(bin) = std::env::var_os("BOARD_UI_TEST_CLI") {
            let out = Command::new(bin)
                .args([
                    "--db",
                    f.db.to_str().unwrap(),
                    "--json",
                    "sources",
                    "show",
                    name,
                ])
                .env("BOARD_USER", "reader")
                .env_remove("BOARD_PROFILE")
                .env_remove("BOARD_TOKEN")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            let source: Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(source["remote"], remote);
            assert_eq!(source["outbound"], false);
        }
    }
    let db = Board::open(&f.db).unwrap();
    let mut settings = Steps::sync();
    settings.tap("source-chat");
    settings.tap("source-configure");
    settings.tap("source-publish");
    settings.tap("source-mirror-roots");
    settings.tap("source-mirror-files");
    settings.tap("source-display-authors");
    settings.tap("source-settings-save");
    settings.render(&f).expect_key("source-settings-confirm");
    assert!(!db.source("reader", "chat").unwrap().outbound);
    settings.tap("source-settings-confirm");
    settings.render(&f).expect_text("Publishing on");
    let chat = db.source("reader", "chat").unwrap();
    assert!(
        chat.outbound
            && chat.settings.mirror_roots
            && chat.settings.mirror_files
            && chat.settings.display_authors
    );
    let mut github = Steps::sync();
    github.tap("source-bugs");
    github.tap("source-configure");
    github.tap("source-gh-pulls");
    github.tap("source-settings-save");
    github.render(&f).expect_key("source-configure");
    let gh = db.source("reader", "bugs").unwrap();
    assert_eq!(gh.settings.github_items, GithubItems::Pulls);
    assert!(!gh.outbound);
}

#[test]
fn source_creation_retains_invalid_input_can_cancel_and_does_not_offer_setup_to_read_only_users() {
    let f = Fixture::new();
    let mut invalid = Steps::sync();
    invalid.create("github", "demo", "not-a-repo", "work");
    let tree = invalid.render(&f);
    tree.expect_key("source-error");
    tree.expect_text("expected GitHub OWNER/REPO");
    invalid.tap("source-cancel");
    let tree = invalid.render(&f);
    assert!(!tree.has_key("source-name"));
    let db = Board::open(&f.db).unwrap();
    assert!(db.sources("reader").unwrap().is_empty());
    db.set_grant("reader", None, "guest", &Rules::parse("work=read").unwrap())
        .unwrap();
    let bearer = db.issue_credential("reader", "guest", None, None).unwrap();
    let steps = Steps::sync();
    let out = f
        .command(json!(steps.0), DESKTOP, &steps.end())
        .env("BOARD_TOKEN", &bearer.token)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let t = f.tree();
    assert!(!t.has_key("source-add"));
    t.expect_text("No sources available. Source settings require read and sync permission in the same channel.");
    db.set_grant(
        "reader",
        None,
        "agent",
        &Rules::parse("work=read,sync").unwrap(),
    )
    .unwrap();
    let bearer = db.issue_credential("reader", "agent", None, None).unwrap();
    let mut steps = Steps::sync();
    steps.create("local", "denied", "queue", "elsewhere");
    let out = f
        .command(json!(steps.0), DESKTOP, &steps.end())
        .env("BOARD_TOKEN", &bearer.token)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    f.tree().expect_key("source-error");
    assert!(db.sources("agent").unwrap().is_empty());
}

#[test]
fn a_stale_open_source_settings_form_cannot_overwrite_another_process() {
    let f = Fixture::new();
    let db = Board::open(&f.db).unwrap();
    db.create_source("reader", "bugs", Provider::Github, "example/repo", "work")
        .unwrap();
    let mut s = Steps::sync();
    s.tap("source-bugs");
    s.tap("source-configure");
    s.tap("source-display-authors");
    s.step(json!({"type":"mark","name":"open-settings"}));
    let mut actions = s.0;
    actions.push(json!({"t":2.1,"type":"tap","key":"source-settings-save"}));
    let tree = f.at_mark(
        &mut f.command(json!(actions), DESKTOP, "2.8"),
        "open-settings",
        || {
            db.patch_source(
                "reader",
                "bugs",
                &SourcePatch {
                    github_items: Some(GithubItems::Issues),
                    ..Default::default()
                },
            )
            .unwrap();
        },
    );
    tree.expect_text(
        "source_changed: Source settings changed. Cancel and reopen settings before saving.",
    );
    let cfg = db.source("reader", "bugs").unwrap();
    assert_eq!(cfg.settings.github_items, GithubItems::Issues);
    assert!(!cfg.settings.display_authors);
}
