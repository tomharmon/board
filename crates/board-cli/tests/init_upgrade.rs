use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::TempDir;

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

#[test]
fn init_leaves_retired_queue_configuration_untouched_and_accepts_full_catalog() {
    let h = Home::new();
    write(
        &h.codex.join("config.toml"),
        format!(
            "[mcp_servers.board]\ncommand={}\nargs=['mcp','--queue']\n",
            json!(BOARD)
        )
        .as_bytes(),
    );
    let path = h.claude.join(".claude.json");
    let full =
        json!({"mcpServers":{"board":{"command":BOARD,"args":["mcp","--full"]}}}).to_string();
    write(&path, full.as_bytes());
    assert!(!h.init("codex", false).status.success());
    success(h.init("claude", false));
    assert_eq!(fs::read_to_string(path).unwrap(), full);
    assert!(
        fs::read_to_string(h.codex.join("config.toml"))
            .unwrap()
            .contains("--queue")
    );
}

#[test]
fn stock_install_upgrades_only_receipted_unchanged_instructions() {
    use sha2::{Digest, Sha256};
    let h = Home::new();
    let slot = h.new_codex();
    let old = "Previous stock instructions\n";
    let previous = format!(
        "{old}\n<!-- board-installed-sha256: {:x} -->\n",
        Sha256::digest(old.as_bytes())
    );
    let previous_crlf = previous.replace('\n', "\r\n");
    write(&slot, previous_crlf.as_bytes());
    assert_eq!(
        success(h.init("codex", true))["harnesses"][0]["skill"],
        "would-upgrade"
    );
    assert_eq!(fs::read_to_string(&slot).unwrap(), previous_crlf);
    let upgraded = success(h.call(&["skill", "install", "codex"]));
    assert_eq!(upgraded["status"], "upgraded");
    let installed = fs::read_to_string(&slot).unwrap();
    assert!(
        installed
            .starts_with(&include_str!("../assets/board-swarm-SKILL.md").replace("\r\n", "\n"))
    );
    assert!(installed.contains("<!-- board-installed-sha256: "));
    assert_eq!(
        success(h.init("codex", false))["harnesses"][0]["skill"],
        "unchanged"
    );

    for custom in [
        old.to_owned(), // A file from an unmarked installation is not Board-owned.
        previous.replace("Previous", "My own"),
        previous.replace("sha256:", "malformed:"),
        previous.trim_end_matches(" -->\n").to_owned(),
    ] {
        write(&slot, custom.as_bytes());
        assert_eq!(
            success(h.init("codex", false))["harnesses"][0]["skill"],
            "customized"
        );
        assert!(!h.call(&["skill", "install", "codex"]).status.success());
        assert_eq!(fs::read_to_string(&slot).unwrap(), custom);
    }
}

#[test]
fn installer_leaves_custom_bundles_and_unrelated_directories_alone() {
    let h = Home::new();
    let unrelated = h.codex.join("skills/board-swarm/SKILL.md");
    write(&unrelated, b"unrelated instructions");
    let notes = h.new_codex().with_file_name("notes.md");
    write(&notes, b"custom bundle");
    assert_eq!(
        success(h.init("codex", false))["harnesses"][0]["skill"],
        "customized"
    );
    assert!(!h.new_codex().exists());
    assert_eq!(fs::read(notes).unwrap(), b"custom bundle");
    assert_eq!(fs::read(unrelated).unwrap(), b"unrelated instructions");
}
