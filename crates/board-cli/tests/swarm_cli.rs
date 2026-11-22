use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::{TempDir, tempdir};

struct WaitChild(Option<Child>);
impl WaitChild {
    fn id(&self) -> u32 {
        self.0.as_ref().unwrap().id()
    }
    fn wait_with_output(mut self) -> std::io::Result<Output> {
        self.0.take().unwrap().wait_with_output()
    }
}
impl Drop for WaitChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct TestBoard {
    tmp: TempDir,
    db: PathBuf,
}

#[test]
fn cli_checkin_handoff_verification_and_operator_decision_form_one_receipt() {
    use serde_json::json;
    let b = TestBoard::new();
    b.start();
    let registered = b.get("", &["swarm", "status"]);
    assert_eq!(registered["workers"][0]["status"], "registered");
    b.get(
        "",
        &[
            "swarm",
            "launch-failed",
            "verify",
            "Harness rejected launch",
        ],
    );
    let started = b.get("verify", &["swarm", "check-in"]);
    assert_eq!(started["workers"][1]["status"], "working");
    assert!(started["workers"][1]["launch_error"].is_null());
    let post = |who, schema, data: Value| {
        b.get(
            who,
            &[
                "swarm",
                "post",
                "--schema",
                schema,
                "--schema-data",
                &data.to_string(),
            ],
        )
    };
    let revision = json!({"kind":"git-commit","value":"a".repeat(40)});
    let h = post(
        "impl",
        "handoff@1",
        json!({"revision":revision,"summary":"Fix split","changes":["src/lib.rs"],"to":"verify"}),
    );
    let id = h["message"]["id"].as_i64().unwrap();
    post(
        "verify",
        "verification@1",
        json!({"handoff":id,"revision":revision,"summary":"All tests passed","checks":[{"name":"unit","status":"pass","command":"cargo test"}]}),
    );
    let d = post(
        "verify",
        "decision-request@1",
        json!({"to":"operator","summary":"Ship this revision?","handoff":id}),
    );
    let attention = b.get("", &["swarm", "attention"]);
    assert_eq!(
        attention
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["action_required"] == true)
            .count(),
        1
    );
    let request = d["message"]["id"].to_string();
    b.get("", &["swarm", "decide", &request, "Approved"]);
    let receipt = b.get("", &["swarm", "receipt"]);
    assert_eq!(receipt["handoffs"][0]["status"], "pass");
    assert!(receipt["decisions"].as_array().unwrap().is_empty());
}
impl TestBoard {
    fn new() -> Self {
        let tmp = tempdir().unwrap();
        let db = tmp.path().join("board.sqlite3");
        Self { tmp, db }
    }
    fn cmd(&self, actor: &str, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_board"));
        c.args(["--json", "--db"])
            .arg(&self.db)
            .args(args)
            .env("BOARD_USER", "operator")
            .env("BOARD_SWARM_RUN", "sample")
            .current_dir(self.tmp.path())
            .env_remove("BOARD_NAME")
            .env_remove("BOARD_TOKEN")
            .env_remove("BOARD_CAN")
            .env_remove("BOARD_PROFILE");
        if !actor.is_empty() {
            c.env("BOARD_PROFILE", actor);
        }
        c
    }
    fn get(&self, actor: &str, args: &[&str]) -> Value {
        parsed(self.cmd(actor, args).output().unwrap())
    }
    fn start(&self) {
        self.get(
            "",
            &["swarm", "start", "Verify the behavior", "--slug", "sample"],
        );
        for name in ["impl", "verify"] {
            let w = self.get(
                "",
                &[
                    "swarm",
                    "add",
                    name,
                    "--role",
                    "implementer",
                    "--brief",
                    "Use a disposable file.",
                ],
            );
            assert_eq!(w["env_vars"]["BOARD_PROFILE"], name);
            assert!(
                Path::new(w["skills_dir"].as_str().unwrap())
                    .join("board-implementer/SKILL.md")
                    .is_file()
            );
        }
    }
    fn wait(&self, who: &str) -> WaitChild {
        let child = WaitChild(Some(
            self.cmd(who, &["swarm", "wait", "--timeout", "20s"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        ));
        let start = Instant::now();
        loop {
            let s = self.get("", &["swarm", "status"]);
            if s["workers"].as_array().unwrap().iter().any(|w| {
                w["name"] == who && w["status"] == "waiting" && w["waiter_pid"] == child.id()
            }) {
                return child;
            }
            if start.elapsed() >= Duration::from_secs(8) {
                panic!("wait did not register: {s}");
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
}
fn parsed(o: Output) -> Value {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}
#[test]
fn role_and_skill_events_record_the_explicit_cli_profile() {
    let b = TestBoard::new();
    b.get(
        "ambient",
        &[
            "--as",
            "editor",
            "roles",
            "create",
            "auditor",
            "--prompt",
            "Check backups.",
        ],
    );
    let dir = b.tmp.path().join("backup-check");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), "Check backups.").unwrap();
    b.get(
        "ambient",
        &["--as", "curator", "skills", "import", dir.to_str().unwrap()],
    );
    let board = board_local::Board::open(&b.db).unwrap();
    let (_, events) = board
        .event_page(0, 10, &Default::default(), Some("role_library.changed"))
        .unwrap();
    let actors: Vec<_> = events.iter().map(|e| e.actor.as_str()).collect();
    assert_eq!(actors, ["operator/editor", "operator/curator"]);
}

#[test]
fn direct_peer_loop_no_coordinator_relay_and_stop_wakes_waiter() {
    let b = TestBoard::new();
    b.start();
    let mut child = b.wait("verify");
    let note = b.get("impl", &["swarm", "post", "verify is working"]);
    assert_eq!(note["notified"], serde_json::json!([]));
    thread::sleep(Duration::from_millis(300));
    assert!(child.0.as_mut().unwrap().try_wait().unwrap().is_none());
    let p = b.get(
        "impl",
        &["swarm", "post", "@verify please check revision A"],
    );
    assert_eq!(p["notified"], serde_json::json!(["verify"]));
    let got = parsed(child.wait_with_output().unwrap());
    assert_eq!(got["status"], "messages");
    assert_eq!(got["messages"][0]["id"], p["message"]["id"]);
    let s = b.get("", &["swarm", "status"]);
    let w = s["workers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == "verify")
        .unwrap();
    assert_eq!(w["status"], "working");
    assert_eq!(w["last_read_cursor"], got["cursor"]);
    // Reply arrives **before** the implementer starts waiting.
    let d = b.get(
        "verify",
        &[
            "swarm",
            "dm",
            "--to",
            "impl",
            "A fails on an empty database",
        ],
    );
    let wake = b.get("impl", &["swarm", "wait", "--timeout", "0s"]);
    assert_eq!(wake["dms"][0]["id"], d["id"]);
    assert_eq!(
        b.get("impl", &["swarm", "wait", "--timeout", "0s"])["status"],
        "still_waiting"
    );
    let child = b.wait("verify");
    b.get("", &["swarm", "stop"]);
    assert_eq!(
        parsed(child.wait_with_output().unwrap())["status"],
        "stopped"
    );
}
#[test]
fn public_and_dm_syntax_is_unambiguous_and_skill_installs_without_opening_db() {
    let b = TestBoard::new();
    let dest = b.tmp.path().join("skills");
    let ds = dest.to_str().unwrap();
    let paths = ["skill", "install", "path", "--dest", ds];
    b.get("", &paths);
    b.get("", &paths);
    let skill = dest.join("board-swarm/SKILL.md");
    assert!(
        fs::read_to_string(&skill)
            .unwrap()
            .starts_with("---\nname: board-swarm\n")
    );
    for (target, dir) in [("codex", ".agents"), ("claude", ".claude")] {
        let installed = parsed(
            b.cmd("", &["skill", "install", target])
                .env("HOME", b.tmp.path())
                .env("USERPROFILE", b.tmp.path())
                .env_remove("CLAUDE_CONFIG_DIR")
                .output()
                .unwrap(),
        );
        let expected = b
            .tmp
            .path()
            .join(format!("{dir}/skills/board-swarm/SKILL.md"));
        assert_eq!(installed["path"], expected.to_str().unwrap());
        assert_eq!(fs::read(expected).unwrap(), fs::read(&skill).unwrap());
    }
    fs::write(&skill, "my customized skill").unwrap();
    assert!(!b.cmd("", &paths).output().unwrap().status.success());
    assert!(!b.db.exists());
    assert_eq!(
        b.cmd("", &["swarm", "post", "hello", "--to", "impl"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        b.cmd("", &["swarm", "dm", "hello"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
    b.start();
    assert!(
        !b.cmd("impl", &["swarm", "stop"])
            .output()
            .unwrap()
            .status
            .success()
    );
}
#[test]
fn actual_git_diff_and_verdict_roundtrip() {
    let b = TestBoard::new();
    b.start();
    let git = |args: &[&str]| {
        let o = Command::new("git")
            .current_dir(b.tmp.path())
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr))
    };
    git(&["init", "-q"]);
    let f = b.tmp.path().join("example.txt");
    fs::write(&f, "old\n").unwrap();
    git(&["add", "example.txt"]);
    git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@localhost",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-qm",
        "base",
    ]);
    fs::write(&f, "new\n").unwrap();
    let p = b.get("impl", &["swarm", "post", "proof", "--diff=HEAD"]);
    let diff = &p["message"]["blocks"][0];
    assert_eq!(diff["schema"], "diff");
    assert_eq!(diff["data"]["files"][0], "example.txt");
    assert!(diff["data"]["patch"].as_str().unwrap().contains("+new"));
    // Putting --diff before the positional text must not swallow the prose as a git revision.
    let diff_first = b.get(
        "impl",
        &[
            "swarm",
            "post",
            "--diff",
            "@verify ready to inspect",
            "--label",
            "phase=review",
            "--idempotency-key",
            "diff-first-key",
        ],
    );
    assert_eq!(diff_first["notified"], serde_json::json!(["verify"]));
    assert_eq!(diff_first["message"]["labels"]["phase"], "review");
    assert_eq!(diff_first["message"]["blocks"][0]["schema"], "diff");
    let retry = b.get(
        "impl",
        &[
            "swarm",
            "post",
            "different body ignored on retry",
            "--idempotency-key",
            "diff-first-key",
        ],
    );
    assert_eq!(retry["message"]["id"], diff_first["message"]["id"]);
    let open_code = b.get(
        "impl",
        &["swarm", "post", "Example:\n```rust\nlet x = 1;", "--diff"],
    );
    assert_eq!(open_code["message"]["blocks"][0]["schema"], "diff");
    fs::write(&f, format!("{}\n", "A".repeat(16000))).unwrap();
    let full = b.get("impl", &["swarm", "post", "--diff"]);
    assert_eq!(full["message"]["attachments"][0]["name"], "changes.diff");
    let v = b.get(
        "verify",
        &[
            "swarm",
            "post",
            "--schema",
            "verdict@1",
            r#"{"status":"pass","summary":"independent check"}"#,
        ],
    );
    assert_eq!(v["message"]["blocks"][0]["data"]["status"], "pass");
    let combined = b.get(
        "verify",
        &[
            "swarm",
            "post",
            "@impl independent check passed",
            "--schema",
            "verdict@1",
            "--schema-data",
            r#"{"status":"pass","summary":"verified with prose mention"}"#,
        ],
    );
    assert_eq!(combined["notified"], serde_json::json!(["impl"]));
    assert_eq!(
        combined["message"]["blocks"][0]["data"]["summary"],
        "verified with prose mention"
    );
}
#[cfg(unix)]
#[test]
fn interrupted_wait_clears_live_pid() {
    let b = TestBoard::new();
    b.start();
    let child = b.wait("impl");
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    assert_eq!(child.wait_with_output().unwrap().status.code(), Some(143));
    let s = b.get("", &["swarm", "status"]);
    let w = s["workers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == "impl")
        .unwrap();
    assert_eq!(w["status"], "working");
    assert!(w["waiter_pid"].is_null());
}

#[test]
fn numeric_and_underscore_worker_profile_can_receive_and_reply() {
    let b = TestBoard::new();
    b.start();
    b.get(
        "",
        &[
            "swarm",
            "add",
            "2_api",
            "--brief",
            "Check API compatibility",
        ],
    );
    let p = b.get("impl", &["swarm", "post", "@2_api check the status code"]);
    assert_eq!(p["notified"], serde_json::json!(["2_api"]));
    let w = b.get("2_api", &["swarm", "wait", "--timeout", "0s"]);
    assert_eq!(w["messages"][0]["id"], p["message"]["id"]);
    let d = b.get(
        "",
        &[
            "--as",
            "2_api",
            "swarm",
            "dm",
            "--to",
            "impl",
            "Confirmed 201",
        ],
    );
    assert_eq!(d["from_actor"], "operator/2_api");
}
