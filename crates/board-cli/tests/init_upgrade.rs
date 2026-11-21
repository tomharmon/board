use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::TempDir;

const PREVIOUS: &str = include_str!("fixtures/coordinator-v15.md");
const CURRENT: &[u8] = include_bytes!("../assets/board-swarm-SKILL.md");
const BOARD: &str = env!("CARGO_BIN_EXE_board");

struct Home {
    dir: TempDir,
    bin: PathBuf,
    claude: PathBuf,
    codex: PathBuf,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("installed tools");
        let claude = dir.path().join("personal Claude");
        let codex = dir.path().join("personal Codex");
        for p in [&bin, &claude, &codex] {
            fs::create_dir(p).unwrap();
        }
        // The registrations are already correct: running these executables as a
        // harness would fail. Only Board itself should run for the MCP preflight.
        for harness in ["codex", "claude"] {
            let exe = bin.join(format!("{harness}{}", std::env::consts::EXE_SUFFIX));
            fs::write(&exe, b"not a runnable harness").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(exe, fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        fs::write(
            claude.join(".claude.json"),
            json!({
                "mcpServers":{"board":{"type":"stdio","command":BOARD,"args":["mcp"]}}
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            codex.join("config.toml"),
            format!(
                "[mcp_servers.board]\ncommand={}\nargs=['mcp']\n",
                json!(BOARD)
            ),
        )
        .unwrap();
        Self {
            dir,
            bin,
            claude,
            codex,
        }
    }
    fn call(&self, args: &[&str]) -> Output {
        Command::new(BOARD)
            .arg("--json")
            .args(args)
            .env("HOME", self.dir.path())
            .env("USERPROFILE", self.dir.path())
            .env("CLAUDE_CONFIG_DIR", &self.claude)
            .env("CODEX_HOME", &self.codex)
            .env("PATH", &self.bin)
            .current_dir(self.dir.path())
            .output()
            .unwrap()
    }
    fn init(&self, harness: &str, dry: bool) -> Output {
        let mut args = vec!["init", "--only", harness, "--board-bin", BOARD];
        if dry {
            args.push("--dry-run");
        }
        self.call(&args)
    }
    fn new_codex(&self) -> PathBuf {
        self.dir.path().join(".agents/skills/board-swarm/SKILL.md")
    }
}

fn write(path: &Path, data: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, data).unwrap();
}
fn success(o: Output) -> Value {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}
fn backups(file: &Path) -> Vec<Vec<u8>> {
    let skills = file.parent().unwrap().parent().unwrap();
    fs::read_dir(skills)
        .unwrap()
        .map(Result::unwrap)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".board-skill-backup-")
        })
        .map(|e| fs::read(e.path().join("coordinator.previous")).unwrap())
        .collect()
}

#[test]
fn init_upgrades_exact_stock_in_place_and_retains_old_bytes() {
    let h = Home::new();
    let slot = h.claude.join("skills/board-swarm/SKILL.md");
    write(&slot, PREVIOUS.as_bytes());
    let plan = success(h.init("claude", true));
    assert_eq!(plan["harnesses"][0]["skill"], "would-upgrade");
    assert_eq!(fs::read(&slot).unwrap(), PREVIOUS.as_bytes());
    assert!(backups(&slot).is_empty());
    let upgraded = success(h.init("claude", false));
    assert_eq!(upgraded["harnesses"][0]["skill"], "upgraded");
    assert_eq!(fs::read(&slot).unwrap(), CURRENT);
    assert_eq!(backups(&slot), [PREVIOUS.as_bytes()]);
    let current = success(h.init("claude", false));
    assert_eq!(current["harnesses"][0]["skill"], "unchanged");
    assert_eq!(backups(&slot).len(), 1);
}

#[test]
fn direct_installer_migrates_legacy_codex_without_leaving_another_active_skill() {
    let h = Home::new();
    let old = h.dir.path().join(".codex/skills/board-swarm/SKILL.md");
    let crlf = PREVIOUS.replace("\r\n", "\n").replace('\n', "\r\n");
    write(&old, crlf.as_bytes());
    let installed = success(h.call(&["skill", "install", "codex"]));
    assert_eq!(installed["status"], "upgraded");
    assert!(!old.exists());
    assert_eq!(fs::read(h.new_codex()).unwrap(), CURRENT);
    assert_eq!(backups(&old), [crlf.as_bytes()]);
    let again = success(h.init("codex", false));
    assert_eq!(again["harnesses"][0]["skill"], "unchanged");
    assert!(!old.exists());
}

#[test]
fn ambiguous_or_customized_legacy_bundles_require_manual_resolution() {
    let h = Home::new();
    let old = h.codex.join("skills/board-swarm/SKILL.md");
    write(&old, b"my coordinator");
    let direct = h.call(&["skill", "install", "codex"]);
    assert!(!direct.status.success());
    assert!(String::from_utf8_lossy(&direct.stderr).contains("custom legacy Codex skill"));
    assert!(!h.init("codex", false).status.success());
    assert!(!h.new_codex().exists());
    assert_eq!(fs::read(&old).unwrap(), b"my coordinator");
    write(&old, PREVIOUS.as_bytes());
    let notes = old.with_file_name("notes.txt");
    write(&notes, b"user customization");
    assert!(!h.call(&["skill", "install", "codex"]).status.success());
    assert!(!h.new_codex().exists());
    assert_eq!(fs::read(&notes).unwrap(), b"user customization");
    write(&h.new_codex(), b"intentional new version");
    assert!(!h.init("codex", true).status.success());
    assert_eq!(fs::read(h.new_codex()).unwrap(), b"intentional new version");
    assert_eq!(fs::read(&old).unwrap(), PREVIOUS.as_bytes());
    assert!(backups(&old).is_empty());
}
