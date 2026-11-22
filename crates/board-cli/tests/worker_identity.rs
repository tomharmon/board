use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

fn board(db: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_board"));
    cmd.env_clear()
        .env("BOARD_USER", "ada")
        .env("BOARD_DB", db)
        .arg("--json");
    cmd
}

#[cfg(unix)]
#[test]
fn invalid_identity_defaults_never_fall_back_to_the_chat_actor() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
    let home = tempfile::tempdir().unwrap();
    for key in ["BOARD_PROFILE", "BOARD_WORKER"] {
        let out = board(&home.path().join("board.sqlite3"))
            .env(key, OsStr::from_bytes(&[0xff]))
            .args(["post", "public", "must not post"])
            .output()
            .unwrap();
        assert!(!out.status.success(), "{key}");
    }
}
struct Mcp {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    seq: u64,
}

#[test]
fn mcp_workers_report_revision_bound_evidence_and_only_the_operator_answers_their_decision() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("workflow.sqlite");
    let mut chat = Mcp::open(board(&db));
    chat.tool(
        "swarm_start",
        json!({"goal":"Verify revision","slug":"audit"}),
        true,
    );
    for name in ["impl", "verify"] {
        chat.tool(
            "swarm_add",
            json!({"run":"audit","name":name,"brief":"Own your slice"}),
            true,
        );
    }
    let worker = |name| {
        let mut command = board(&db);
        command.args(["--worker", name]);
        Mcp::open(command)
    };
    let mut implementer = worker("audit/impl");
    let mut verifier = worker("audit/verify");
    implementer.tool("swarm_check_in", json!({}), true);
    let revision = json!({"kind":"sha256","value":"a".repeat(64)});
    let h=implementer.tool("swarm_post",json!({"text":"Ready","schema":"handoff@1","schema_data":{"revision":revision,"summary":"Unicode fix","changes":["fields.rs"],"to":"verify"}}),true);
    let handoff = h["message"]["id"].as_i64().unwrap();
    verifier.tool("swarm_post",json!({"text":"Verified","schema":"verification@1","schema_data":{"handoff":handoff,"revision":revision,"summary":"Passed","checks":[{"name":"unit","status":"pass","command":"cargo test"}]}}),true);
    let d=verifier.tool("swarm_post",json!({"text":"Decision needed","schema":"decision-request@1","schema_data":{"to":"ada","summary":"Accept this change?","handoff":handoff}}),true);
    let request = d["message"]["id"].as_i64().unwrap();
    implementer.tool(
        "swarm_decide",
        json!({"id":request,"summary":"Self-approval"}),
        false,
    );
    chat.tool(
        "swarm_decide",
        json!({"run":"audit","id":request,"summary":"Accepted"}),
        true,
    );
    let receipt = chat.tool("swarm_receipt", json!({"run":"audit"}), true);
    assert_eq!(receipt["handoffs"][0]["status"], "pass");
    assert_eq!(receipt["handoffs"][0]["verified_by"], "ada/verify");
    assert!(receipt["decisions"].as_array().unwrap().is_empty());
    chat.tool("swarm_stop", json!({"run":"audit"}), true);
    let create = board(&db)
        .args([
            "--worker",
            "audit/impl",
            "tasks",
            "create",
            "other",
            "Cannot acquire",
            "--claim",
        ])
        .output()
        .unwrap();
    assert!(!create.status.success());
    assert!(String::from_utf8_lossy(&create.stderr).contains("swarm_stopped"));
}
impl Mcp {
    fn open(mut cmd: Command) -> Self {
        let mut child = cmd
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut m = Self {
            child,
            input,
            output,
            seq: 0,
        };
        assert_eq!(
            m.rpc("initialize", json!({}))["serverInfo"]["name"],
            "board-mcp"
        );
        m
    }
    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.seq += 1;
        writeln!(
            self.input.as_mut().unwrap(),
            "{}",
            json!({"jsonrpc":"2.0","id":self.seq,"method":method,"params":params})
        )
        .unwrap();
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        let frame: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(frame["id"], self.seq);
        frame["result"].clone()
    }
    fn tool(&mut self, name: &str, args: Value, ok: bool) -> Value {
        let v = self.rpc("tools/call", json!({"name":name,"arguments":args}));
        assert_eq!(v["isError"], !ok, "{name}: {v}");
        serde_json::from_str(v["content"][0]["text"].as_str().unwrap()).unwrap()
    }
}
impl Drop for Mcp {
    fn drop(&mut self) {
        drop(self.input.take());
        let _ = self.child.wait();
    }
}

#[test]
fn workers_use_their_assignment_and_never_rebind_the_shared_chat() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("board test.sqlite3");
    let mut chat = Mcp::open(board(&db));
    chat.tool(
        "swarm_start",
        json!({"goal":"Check two roles","slug":"release"}),
        true,
    );
    chat.tool(
        "swarm_start",
        json!({"goal":"Other run","slug":"other"}),
        true,
    );
    for (name, role) in [("build", "implementer"), ("check", "verifier")] {
        let spawn = chat.tool(
            "swarm_add",
            json!({"run":"release","name":name,"role":role,"brief":"Test the saved revision"}),
            true,
        );
        let actor = format!("ada/{name}");
        let selector = format!("release/{name}");
        assert_eq!(spawn["env_vars"]["BOARD_WORKER"], selector);
        assert!(spawn["env_vars"].get("BOARD_TOKEN").is_none());
        let prefix: Vec<String> = serde_json::from_value(spawn["cli_args"].clone()).unwrap();
        assert!(
            spawn["assembled_prompt"]
                .as_str()
                .unwrap()
                .contains("--worker")
        );
        assert!(
            Path::new(spawn["skills_dir"].as_str().unwrap())
                .join(format!("board-{role}/SKILL.md"))
                .is_file()
        );
        // Subagents can inherit the entire coordinator environment and no child overrides.
        let scoped = || {
            let mut c = board(&db);
            c.args(&prefix)
                .env("BOARD_PROFILE", "chat")
                .env("BOARD_SWARM_RUN", "other")
                .env("BOARD_USER", "other-person");
            c
        };
        let who = scoped().arg("whoami").output().unwrap();
        let who: Value = serde_json::from_slice(&who.stdout).unwrap();
        assert_eq!(who["actor"], actor);
        assert_eq!(who["worker"]["run"], "release");
        let result = scoped()
            .args(["swarm", "post", "verified independently"])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let post: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(post["message"]["actor"], actor);
        for bad in [
            &["swarm", "stop", "release"][..],
            &["swarm", "post", "wrong", "--run", "other"],
            &["--as", "admin", "post", "any", "wrong"],
            &["swarm", "start", "not my decision"],
            &["roles", "edit", "verifier", "--prompt", "wrong"],
        ] {
            assert!(
                !scoped().args(bad).output().unwrap().status.success(),
                "{bad:?}"
            );
        }
        let mut own = Mcp::open(scoped());
        let advertised: Vec<_> = own.rpc("tools/list", json!({}))["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect();
        for hidden in [
            "swarm_start",
            "swarm_list",
            "swarm_add",
            "swarm_plan",
            "swarm_set_limit",
            "swarm_stop",
            "roles_list",
            "roles_show",
        ] {
            assert!(
                !advertised.iter().any(|n| n == hidden),
                "worker MCP should not advertise {hidden}"
            );
        }
        let who = own.tool("board_whoami", json!({}), true);
        assert_eq!(who["actor"], actor);
        assert_eq!(who["worker"]["name"], name);
        own.tool("swarm_start", json!({"goal":"forbidden"}), false);
        own.tool("swarm_set_limit", json!({"max_notifications":50}), false);
        own.tool("roles_list", json!({}), false);
        own.tool("swarm_status", json!({"run":"other"}), false);
        own.tool(
            "swarm_post",
            json!({"text":"using a dedicated connection"}),
            true,
        );
    }
    assert!(
        !board(&db)
            .args(["--worker", "release/unassigned", "whoami"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let run = |args: &[&str]| {
        let out = board(&db).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<Value>(&out.stdout).unwrap()
    };
    run(&["grants", "create", "ada", "--can", "swarms/release=all"]);
    let key = run(&["credentials", "issue", "ada/build"]);
    let token = key["token"].as_str().unwrap();
    let scoped = |selector: &str| {
        board(&db)
            .env("BOARD_TOKEN", token)
            .args(["--worker", selector, "whoami"])
            .output()
            .unwrap()
    };
    assert!(scoped("release/build").status.success());
    assert!(!scoped("release/check").status.success());
    let shared = chat.tool("board_whoami", json!({}), true);
    assert_eq!(shared["actor"], "ada");
    assert!(shared["worker"].is_null());
    assert_eq!(
        chat.tool("swarm_status", json!({"run":"release"}), true)["status"],
        "active"
    );
}
