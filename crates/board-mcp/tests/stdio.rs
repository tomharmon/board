use board_local::{Board, access::Rules};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

struct Client {
    child: Child,
    input: ChildStdin,
    lines: mpsc::Receiver<String>,
    id: u64,
}
impl Client {
    fn start(path: &Path, profile: Option<&str>, token: Option<&str>) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_board-mcp-stdio"));
        cmd.env_clear()
            .env("BOARD_DB", path)
            .env("BOARD_USER", "ada")
            .env("BOARD_SWARM_RUN", "release");
        if let Some(profile) = profile {
            cmd.env("BOARD_PROFILE", profile);
        }
        if let Some(token) = token {
            cmd.env("BOARD_TOKEN", token);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            lines,
            id: 0,
        }
    }
    fn write(&mut self, s: &str) {
        writeln!(self.input, "{s}").unwrap();
        self.input.flush().unwrap();
    }
    fn read(&self) -> Value {
        let line = self.lines.recv_timeout(Duration::from_secs(10)).expect(
            "expected one complete protocol frame; no diagnostic output is permitted on stdout",
        );
        serde_json::from_str(&line).unwrap()
    }
    fn send(&mut self, method: &str, params: Value) -> u64 {
        self.id += 1;
        self.write(
            &json!({"jsonrpc":"2.0","method":method,"id":self.id,"params":params}).to_string(),
        );
        self.id
    }
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params);
        let msg = self.read();
        assert_eq!(msg["id"], id, "{msg}");
        msg
    }
    fn initialize(&mut self) {
        let r=self.request("initialize",json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"pipe-test","version":"1"}}));
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(r["result"]["serverInfo"]["name"], "board-mcp");
        assert_eq!(r["result"]["capabilities"], json!({"tools":{}}));
        self.write(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        assert_eq!(self.request("ping", json!({}))["result"], json!({}));
    }
    fn tool(&mut self, name: &str, args: Value, success: bool) -> Value {
        let reply = self.request("tools/call", json!({"name":name,"arguments":args}));
        Self::content(reply, success)
    }
    fn content(reply: Value, success: bool) -> Value {
        assert_eq!(reply["result"]["isError"], !success, "{reply}");
        assert_eq!(reply["result"]["content"][0]["type"], "text");
        serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    }
    fn call(&mut self, name: &str, args: Value) -> Value {
        self.tool(name, args, true)
    }
    fn denied(&mut self, name: &str, args: Value) -> Value {
        self.tool(name, args, false)
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn protocol_framing_inventory_and_schema_validation() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = Client::start(&tmp.path().join("board"), None, None);
    c.write("not JSON");
    assert_eq!(c.read()["error"]["code"], -32700);
    c.write("[]");
    assert_eq!(c.read()["error"]["code"], -32600);
    assert_eq!(c.request("tools/list", json!({}))["error"]["code"], -32002);
    c.initialize();
    c.write(&"x".repeat(2 * 1024 * 1024 + 1));
    assert_eq!(c.read()["error"]["code"], -32700);
    // A client notification cannot execute a mutating tool, even when it names tools/call.
    c.write(r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"board_post","arguments":{"channel":"unexpected","text":"oops"}}}"#);
    assert!(
        c.call("board_channels", json!({}))["channels"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let tools = c.request("tools/list", json!({}))["result"]["tools"]
        .as_array()
        .unwrap()
        .clone();
    let mut names: Vec<_> = tools
        .iter()
        .map(|t| {
            assert_eq!(t["inputSchema"]["additionalProperties"], false);
            assert_eq!(t["inputSchema"]["type"], "object");
            t["name"].as_str().unwrap()
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "board_channels",
            "board_post",
            "board_read",
            "board_reply",
            "board_whoami",
            "roles_list",
            "roles_show",
            "swarm_add",
            "swarm_dm",
            "swarm_done",
            "swarm_post",
            "swarm_read",
            "swarm_start",
            "swarm_status",
            "swarm_stop",
            "swarm_wait",
            "tasks_block",
            "tasks_claim",
            "tasks_complete",
            "tasks_create",
            "tasks_fail",
            "tasks_list"
        ]
    );
    assert_eq!(c.request("unsupported", json!({}))["error"]["code"], -32601);
    assert_eq!(
        c.request("tools/call", json!({"name":"bogus"}))["error"]["code"],
        -32602
    );
    c.denied("swarm_post", json!({"text":"public","to":"worker"}));
    c.denied("swarm_dm", json!({"text":"missing target"}));
    c.denied("swarm_wait", json!({"timeout_seconds":301}));
    c.denied("tasks_claim", json!({"id":1,"ready_channel":"bugs"}));
    c.denied(
        "board_post",
        json!({"channel":"bugs","text":"no impersonation","actor":"another"}),
    );
    c.denied("board_read", json!({"limit":0}));
}

#[test]
fn integer_run_reference_cannot_stop_a_different_numeric_slug() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = Client::start(&tmp.path().join("board"), None, None);
    c.initialize();
    let first = c.call("swarm_start", json!({"goal":"first", "slug":"audit"}));
    let slug = first["id"].to_string();
    c.call("swarm_start", json!({"goal":"second", "slug":slug}));
    assert_eq!(
        c.call("swarm_stop", json!({"run":first["id"]}))["slug"],
        "audit"
    );
    let second = c.call("swarm_status", json!({"run":slug}));
    assert_eq!(second["goal"], "second");
    assert_eq!(second["status"], "active");
}

#[test]
fn an_open_markdown_example_cannot_swallow_a_requested_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = Client::start(&tmp.path().join("board"), None, None);
    c.initialize();
    c.call("swarm_start", json!({"goal":"test", "slug":"markdown"}));
    let input = |data: Value| json!({"run":"markdown","text":"My example:\n```rust\nlet x = 1;", "schema":"verdict@1", "schema_data":data});
    c.denied("swarm_post", input(json!({"status":"no-such-verdict"})));
    let post = c.call(
        "swarm_post",
        input(json!({"status":"pass", "summary":"works"})),
    );
    assert_eq!(post["message"]["blocks"][0]["data"]["status"], "pass");
    assert!(
        post["message"]["text"]
            .as_str()
            .unwrap()
            .contains("```rust\nlet x = 1;")
    );
}

#[test]
fn processes_exchange_public_posts_private_dms_and_live_wakeups_without_launching_workers() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("board");
    let mut coordinator = Client::start(&path, None, None);
    coordinator.initialize();
    let roles = coordinator.call("roles_list", json!({}));
    assert!(roles.as_array().unwrap().len() >= 6);
    assert_eq!(
        coordinator.call("roles_show", json!({"slug":"verifier"}))["slug"],
        "verifier"
    );
    let run = coordinator.call(
        "swarm_start",
        json!({"goal":"Check receipt recovery","slug":"release"}),
    );
    let id = run["id"].as_i64().unwrap();
    let spawn = coordinator.call(
        "swarm_add",
        json!({"run":id,"name":"checker","role":"verifier","brief":"Verify the receipt"}),
    );
    assert_eq!(spawn["worker"]["actor"], "ada/checker");
    assert!(Path::new(spawn["skills_dir"].as_str().unwrap()).is_dir());
    assert!(
        spawn["assembled_prompt"]
            .as_str()
            .unwrap()
            .contains("Verify the receipt")
    );
    let mut checker = Client::start(&path, Some("checker"), None);
    checker.initialize();
    let observer = board_local::Board::open(&path).unwrap();
    let before = (observer.cursor().unwrap(), observer.data_version().unwrap());
    let timeout = checker.call("swarm_wait", json!({"timeout_seconds":0}));
    assert_eq!(timeout["status"], "still_waiting");
    assert_eq!(
        (observer.cursor().unwrap(), observer.data_version().unwrap()),
        before,
        "a zero-time check on an empty inbox must not register a waiter or commit a write"
    );
    let req = checker.send(
        "tools/call",
        json!({"name":"swarm_wait","arguments":{"timeout_seconds":5}}),
    );
    let until = Instant::now() + Duration::from_secs(4);
    while coordinator.call("swarm_status", json!({}))["workers"][0]["status"] != "waiting" {
        assert!(
            Instant::now() < until,
            "worker failed to register its deadline"
        );
        thread::sleep(Duration::from_millis(15));
    }
    let posted=coordinator.call("swarm_post",json!({"text":"Please check, @checker","schema":"verdict@1","schema_data":{"status":"not-run","summary":"Awaiting checker"}}));
    assert_eq!(posted["notified"], json!(["checker"]));
    assert_eq!(posted["message"]["blocks"].as_array().unwrap().len(), 1);
    let line = checker.read();
    assert_eq!(line["id"], req);
    let delivered = Client::content(line, true);
    assert_eq!(delivered["status"], "messages");
    assert_eq!(delivered["messages"][0]["id"], posted["message"]["id"]);
    assert_eq!(
        coordinator.call("swarm_status", json!({}))["workers"][0]["status"],
        "working"
    );
    let dm = checker.call(
        "swarm_dm",
        json!({"to":"coordinator","text":"Receipts verified"}),
    );
    let received = coordinator.call("swarm_wait", json!({"timeout_seconds":0}));
    assert_eq!(received["dms"][0]["id"], dm["id"]);
    let feed = coordinator.call("swarm_read", json!({"after":delivered["cursor"],"limit":5}));
    assert!(
        feed["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["dm"]["id"] == dm["id"])
    );
    assert_eq!(
        checker.denied("swarm_stop", json!({}))["code"],
        "swarm_coordinator_required"
    );
    assert_eq!(
        checker.call("swarm_done", json!({"summary":"Done"}))["status"],
        "done"
    );
    assert_eq!(
        coordinator.call("swarm_stop", json!({}))["status"],
        "stopped"
    );
    assert_eq!(
        checker.call("swarm_wait", json!({"timeout_seconds":0}))["status"],
        "stopped"
    );
}

#[test]
fn a_restarted_stdio_waiter_recovers_an_unread_mention_after_hard_termination() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board");
    let mut boss = Client::start(&path, None, None);
    boss.initialize();
    boss.call(
        "swarm_start",
        json!({"slug":"release","goal":"Park and recover"}),
    );
    boss.call(
        "swarm_add",
        json!({"name":"checker","brief":"Verify offline"}),
    );
    let mut first = Client::start(&path, Some("checker"), None);
    first.initialize();
    first.send(
        "tools/call",
        json!({"name":"swarm_wait","arguments":{"timeout_seconds":30}}),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while boss.call("swarm_status", json!({}))["workers"][0]["waiter_pid"] != first.child.id() {
        assert!(Instant::now() < deadline, "waiter did not enter");
        thread::sleep(Duration::from_millis(15));
    }
    first.child.kill().unwrap();
    first.child.wait().unwrap();
    let message = boss.call(
        "swarm_post",
        json!({"text":"@checker verify this revision"}),
    )["message"]["id"]
        .clone();
    let mut replacement = Client::start(&path, Some("checker"), None);
    replacement.initialize();
    let result = replacement.call("swarm_wait", json!({"timeout_seconds":0}));
    assert_eq!(result["status"], "messages");
    assert_eq!(result["cursor"], message);
    assert_eq!(result["messages"][0]["id"], message);
    assert_eq!(
        replacement.call("swarm_wait", json!({"timeout_seconds":0}))["status"],
        "still_waiting"
    );
    let worker = boss.call("swarm_status", json!({}))["workers"][0].clone();
    assert_eq!(worker["status"], "working");
    assert!(worker["waiter_pid"].is_null());
}

#[test]
fn messages_fenced_task_transitions_and_permission_errors_share_the_board() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("board");
    let mut owner = Client::start(&path, None, None);
    owner.initialize();
    let root = owner.call(
        "board_post",
        json!({"channel":"bugs","text":"reproduction","labels":{"kind":"issue"},"no_sync":true}),
    );
    let reply = owner.call("board_reply", json!({"id":root["id"],"text":"second step"}));
    assert_eq!(
        owner.call("board_read", json!({"target":reply["id"]}))["replies"][0]["id"],
        reply["id"]
    );
    assert_eq!(
        owner.call(
            "board_read",
            json!({"channel":"bugs","text":"second","by":"me"})
        )["rows"][0]["id"],
        reply["id"]
    );
    assert_eq!(
        owner.call("board_channels", json!({"prefix":"bu","limit":1}))["channels"][0]["name"],
        "bugs"
    );
    let created = owner.call(
        "tasks_create",
        json!({"channel":"bugs","text":"fix receipt","labels":{"priority":"next"}}),
    );
    let id = &created["id"];
    assert_eq!(
        owner.call(
            "tasks_list",
            json!({"channel":"bugs","state":"ready","labels":{"priority":"next"}})
        )["tasks"][0]["id"],
        *id
    );
    let mut worker = Client::start(&path, Some("dev"), None);
    worker.initialize();
    assert_eq!(
        worker.call(
            "tasks_claim",
            json!({"ready_channel":"bugs","labels":{"priority":"next"},"lease_seconds":30})
        )["id"],
        *id
    );
    let mut other = Client::start(&path, Some("dev"), None);
    other.initialize();
    assert_eq!(
        other.denied("tasks_complete", json!({"id":id,"result":"not my lease"}))["code"],
        "claim_lost"
    );
    let done = worker.call("tasks_complete", json!({"id":id,"result":"verified"}));
    assert_eq!(done["state"], "completed");
    assert_eq!(done["result"], "verified");
    let failing = worker.call(
        "tasks_create",
        json!({"channel":"bugs","text":"broken","claim":true}),
    );
    assert_eq!(
        worker.call(
            "tasks_fail",
            json!({"id":failing["id"],"reason":"fixture unavailable"})
        )["state"],
        "failed"
    );
    let blocking = worker.call(
        "tasks_create",
        json!({"channel":"bugs","text":"wait","needs":[id],"claim":true}),
    );
    assert_eq!(
        worker.call(
            "tasks_block",
            json!({"id":blocking["id"],"reason":"input needed"})
        )["state"],
        "blocked"
    );

    let board = Board::open(&path).unwrap();
    board
        .set_grant("ada", None, "guest", &Rules::parse("bugs=read").unwrap())
        .unwrap();
    let secret = board.issue_credential("ada", "guest", None, None).unwrap();
    let mut reader = Client::start(&path, None, Some(&secret.token));
    reader.initialize();
    reader.call("board_read", json!({"target":id}));
    reader.denied(
        "board_post",
        json!({"channel":"bugs","text":"must not write"}),
    );
    reader.denied("roles_show", json!({"slug":"verifier"}));
    board
        .revoke_credential("ada", &secret.credential.id)
        .unwrap();
    reader.denied("board_read", json!({"target":id}));
}
#[cfg(unix)]
#[test]
fn invalid_worker_setting_does_not_open_a_coordinator_connection() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
    let dir = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_board-mcp-stdio"))
        .env_clear()
        .env("BOARD_DB", dir.path().join("board.sqlite3"))
        .env("BOARD_WORKER", OsStr::from_bytes(&[0xff]))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("BOARD_WORKER"));
}
