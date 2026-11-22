use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};
use tempfile::TempDir;

struct Home {
    root: TempDir,
    bin: PathBuf,
    board: PathBuf,
}
impl Home {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("installed tools");
        fs::create_dir(&bin).unwrap();
        let board = bin.join(format!("board{}", std::env::consts::EXE_SUFFIX));
        // The stable Homebrew-style link also keeps concurrent tests from copying
        // a full debug executable per disposable home.
        #[cfg(unix)]
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_board"), &board).unwrap();
        #[cfg(not(unix))]
        fs::hard_link(env!("CARGO_BIN_EXE_board"), &board)
            .or_else(|_| fs::copy(env!("CARGO_BIN_EXE_board"), &board).map(|_| ()))
            .unwrap();
        Self { root, bin, board }
    }
    fn cmd(&self, args: &[&str]) -> Output {
        self.run(true, args)
    }
    fn run(&self, json: bool, args: &[&str]) -> Output {
        let mut cmd = Command::new(&self.board);
        if json {
            cmd.arg("--json");
        }
        cmd.arg("init")
            .args(args)
            .current_dir(self.root.path())
            .env("HOME", self.root.path())
            .env("USERPROFILE", self.root.path())
            .env("PATH", &self.bin)
            .env("BOARD_DB", self.root.path().join("do-not-migrate.db"));
        for v in [
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
            "BOARD_PROFILE",
            "BOARD_TOKEN",
            "BOARD_NAME",
            "BOARD_WORKER",
            "BOARD_CAN",
            "BOARD_SWARM_RUN",
        ] {
            cmd.env_remove(v);
        }
        // Some Linux filesystems briefly report ETXTBSY after creating an executable.
        for _ in 0..20 {
            match cmd.output() {
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(std::time::Duration::from_millis(10))
                }
                other => return other.unwrap(),
            }
        }
        cmd.output().unwrap()
    }
    #[cfg(unix)]
    fn harness(&self, name: &str) {
        use std::os::unix::fs::PermissionsExt;
        let text = match name {
            "codex" => {
                r#"#!/bin/sh
set -eu
test "$1 $2 $3 $4" = 'mcp add board --'
/bin/mkdir -p "$HOME/.codex"
printf '[mcp_servers.board]\ncommand="%s"\nargs=["mcp"]\n' "$5" >> "$HOME/.codex/config.toml"
"#
            }
            "claude" => {
                r#"#!/bin/sh
set -eu
test "$1 $2 $3 $4 $5 $6 $7 $8" = 'mcp add --scope user --transport stdio board --'
printf '{"mcpServers":{"board":{"type":"stdio","command":"%s","args":["mcp"]}}}' "$9" > "$HOME/.claude.json"
"#
            }
            _ => panic!(),
        };
        let p = self.bin.join(name);
        fs::write(&p, text).unwrap();
        fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fn skill(&self, base: &str) -> PathBuf {
        self.root
            .path()
            .join(base)
            .join("skills/board-swarm/SKILL.md")
    }
}
fn json(o: Output) -> Value {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}
fn state<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["harnesses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["name"] == name)
        .unwrap()
}
#[test]
fn unknown_clients_get_manual_setup_without_touching_the_board() {
    let h = Home::new();
    let plan = json(h.cmd(&["--dry-run"]));
    assert_eq!(plan["server_verified"], false);
    let v = json(h.cmd(&[]));
    assert_eq!(v["server_verified"], true);
    assert_eq!(v["manual"]["command"], serde_json::json!(h.board));
    assert_eq!(v["manual"]["args"], serde_json::json!(["mcp"]));
    for e in v["harnesses"].as_array().unwrap() {
        assert_eq!(e["detected"], false);
        assert_eq!(e["verified"], false);
        assert_eq!(e["registration"], "not-installed");
    }
    let out = h.run(false, &[]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("verified over stdio") && text.contains("No client was configured"));
    assert!(text.contains("Other harnesses: register stdio command"));
    assert!(!text.contains("Open a new coding chat"));
    for path in [".codex", ".claude", ".claude.json", ".agents"] {
        assert!(!h.root.path().join(path).exists());
    }
    assert!(!h.root.path().join("do-not-migrate.db").exists());
}

#[cfg(unix)]
#[test]
fn an_inactive_claude_profile_does_not_mask_missing_user_registration() {
    let h = Home::new();
    h.harness("claude");
    let unused = h.root.path().join(".claude/.claude.json");
    fs::create_dir(unused.parent().unwrap()).unwrap();
    fs::write(&unused,serde_json::json!({"mcpServers":{"board":{"type":"stdio","command":h.board,"args":["mcp"]}}}).to_string()).unwrap();
    let original = fs::read(&unused).unwrap();
    let report = json(h.cmd(&[]));
    assert_eq!(state(&report, "claude")["registration"], "registered");
    assert!(h.root.path().join(".claude.json").exists());
    assert_eq!(fs::read(unused).unwrap(), original);
}
#[cfg(unix)]
#[test]
fn discovery_dry_run_real_handshake_and_safe_repeated_init() {
    let h = Home::new();
    h.harness("codex");
    let plan = json(h.cmd(&["--dry-run"]));
    assert_eq!(state(&plan, "codex")["detected"], true);
    assert!(!h.skill(".agents").exists());
    assert!(!h.root.path().join(".codex/config.toml").exists());
    let first = json(h.cmd(&[]));
    assert_eq!(state(&first, "codex")["verified"], true);
    assert!(h.skill(".agents").is_file());
    let config = h.root.path().join(".codex/config.toml");
    let before = fs::read(&config).unwrap();
    fs::write(h.skill(".agents"), "my own skill").unwrap();
    h.harness("claude");
    let second = json(h.cmd(&[]));
    assert_eq!(state(&second, "codex")["skill"], "customized");
    assert_eq!(state(&second, "claude")["verified"], true);
    assert!(h.skill(".claude").is_file());
    assert_eq!(fs::read(config).unwrap(), before);
    assert_eq!(fs::read(h.skill(".agents")).unwrap(), b"my own skill");
    assert!(!h.root.path().join("do-not-migrate.db").exists());
}
#[cfg(unix)]
#[test]
fn foreign_or_disabled_registration_is_neither_run_nor_replaced() {
    let h = Home::new();
    h.harness("codex");
    let config = h.root.path().join(".codex/config.toml");
    fs::create_dir(config.parent().unwrap()).unwrap();
    let same = format!(
        "[mcp_servers.board]\ncommand='{}'\nargs=['mcp']\n",
        h.board.display()
    );
    for content in [
        String::from("[mcp_servers.board]\nurl='https://invalid.example'\n"),
        format!("{same}enabled=false\n"),
        format!("{same}cwd='/missing'\n"),
        format!("{same}disabled_tools=['swarm_add']\n"),
        "[mcp_servers.board]\nBROKEN = 'canary-private-value".into(),
    ] {
        fs::write(&config, &content).unwrap();
        let out = h.cmd(&[]);
        assert!(!out.status.success());
        assert!(!String::from_utf8_lossy(&out.stdout).contains("canary-private-value"));
        assert_eq!(fs::read_to_string(&config).unwrap(), content);
        assert!(!h.skill(".agents").exists());
    }
}

#[cfg(unix)]
#[test]
fn wrong_executable_without_swarm_start_is_rejected_before_writing_config() {
    use std::os::unix::fs::PermissionsExt;
    let h = Home::new();
    let bin = h.bin.join("not-board");
    fs::write(&bin,"#!/bin/sh\n/bin/cat >/dev/null\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"serverInfo\":{\"name\":\"board-mcp\"}}}' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}'\n").unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    let exe = bin.to_str().unwrap();
    assert_eq!(
        json(h.cmd(&["--board-bin", exe, "--dry-run"]))["server_verified"],
        false
    );
    for codex_installed in [false, true] {
        if codex_installed {
            h.harness("codex");
        }
        let out = h.cmd(&["--board-bin", exe]);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("swarm_start"));
    }
    assert!(!h.root.path().join("do-not-migrate.db").exists());
    assert!(!h.skill(".agents").exists());
    assert!(!h.root.path().join(".codex/config.toml").exists());
}
