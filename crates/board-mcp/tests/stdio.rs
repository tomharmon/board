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
        Self::start_session(path, profile, token, None)
    }
    fn start_session(
        path: &Path,
        profile: Option<&str>,
        token: Option<&str>,
        worker: Option<&str>,
    ) -> Self {
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
        if let Some(worker) = worker {
            cmd.env("BOARD_WORKER", worker);
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
            "board_attention",
            "board_channels",
            "board_file_read",
            "board_file_upload",
            "board_post",
            "board_read",
            "board_reply",
            "board_whoami",
            "roles_list",
            "roles_show",
            "swarm_add",
            "swarm_check_in",
            "swarm_decide",
            "swarm_dm",
            "swarm_done",
            "swarm_launch_failed",
            "swarm_list",
            "swarm_observe",
            "swarm_post",
            "swarm_read",
            "swarm_receipt",
            "swarm_set_limit",
            "swarm_start",
            "swarm_status",
            "swarm_stop",
            "swarm_wait",
            "tasks_block",
            "tasks_claim",
            "tasks_complete",
            "tasks_create",
            "tasks_fail",
            "tasks_list",
            "tasks_plan",
            "tasks_release",
            "tasks_renew"
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
fn mcp_file_chunks_require_a_live_readable_placement_on_every_read() {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board");
    let b = Board::open(&path).unwrap();
    b.set_grant("ada", None, "guest", &Rules::parse("shared=read").unwrap())
        .unwrap();
    let token = b.issue_credential("ada", "guest", None, None).unwrap();
    let mut reader = Client::start(&path, None, Some(&token.token));
    let mut owner = Client::start(&path, None, None);
    reader.initialize();
    owner.initialize();
    let mut upload = json!({"channel":"private","name":"result.bin","content_type":"application/octet-stream","data_base64":""});
    reader.denied("board_file_upload", upload.clone());
    let empty = owner.call("board_file_upload", upload.clone());
    let part = owner.call("board_file_read", json!({"id":empty["id"]}));
    assert_eq!(part["data_base64"], "");
    assert!(part["next_offset"].is_null());
    upload["data_base64"] = json!("not base64!");
    owner.denied("board_file_upload", upload.clone());
    let mut bytes = vec![0xB1; board_local::FILE_CHUNK_BYTES + 1];
    upload["data_base64"] = json!(STANDARD.encode(&bytes));
    owner.denied("board_file_upload", upload.clone());
    bytes.pop();
    upload["data_base64"] = json!(STANDARD.encode(&bytes));
    let attachment = owner.call("board_file_upload", upload);
    let id = &attachment["id"];
    let slice = json!({"id":id,"length":3});
    assert_eq!(
        reader.denied("board_file_read", slice.clone())["code"],
        "attachment_not_found"
    );
    owner.call(
        "board_post",
        json!({"channel":"private","text":"Private copy","attachment_ids":[id]}),
    );
    assert_eq!(
        reader.denied("board_file_read", slice.clone())["code"],
        "attachment_not_found"
    );
    let shared = owner.call(
        "board_post",
        json!({"channel":"shared","text":"Published result","attachment_ids":[id]}),
    );
    let part = reader.call("board_file_read", slice);
    assert_eq!(part["attachment"], attachment);
    assert_eq!(part["offset"], 0);
    assert_eq!(part["next_offset"], 3);
    let mut got = STANDARD
        .decode(part["data_base64"].as_str().unwrap())
        .unwrap();
    let rest = reader.call(
        "board_file_read",
        json!({"id":id,"offset":part["next_offset"]}),
    );
    assert!(rest["next_offset"].is_null());
    got.extend(
        STANDARD
            .decode(rest["data_base64"].as_str().unwrap())
            .unwrap(),
    );
    assert_eq!(got, bytes);
    assert!(
        reader.call("board_file_read", json!({"id":id,"offset":bytes.len()}))["next_offset"]
            .is_null()
    );
    for bad in [
        json!({"id":id,"length":0}),
        json!({"id":id,"length":65537}),
        json!({"id":id,"offset":-1}),
        json!({"id":id,"offset":bytes.len()+1}),
    ] {
        reader.denied("board_file_read", bad);
    }
    b.delete(shared["id"].as_i64().unwrap(), "ada", None)
        .unwrap();
    assert_eq!(
        reader.denied(
            "board_file_read",
            json!({"id":id,"offset":part["next_offset"]})
        )["code"],
        "attachment_not_found"
    );
}

#[test]
fn a_parked_worker_can_renew_cancel_and_claim_a_newly_ready_task() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("pool");
    let mut boss = Client::start(&db, None, None);
    let mut worker = Client::start(&db, Some("pool"), None);
    boss.initialize();
    worker.initialize();
    let first = boss.call(
        "tasks_create",
        json!({"channel":"pool","text":"first","claim":true}),
    );
    let next = boss.call(
        "tasks_create",
        json!({"channel":"pool","text":"second","needs":[first["id"]]}),
    );
    let mine = worker.call(
        "tasks_create",
        json!({"channel":"other","text":"mine","claim":true}),
    );
    let req = json!({"name":"tasks_claim","arguments":{"ready_channel":"pool","wait_seconds":5}});
    let parked = worker.send("tools/call", req.clone());
    let renewed = worker.call("tasks_renew", json!({"id":mine["id"],"lease_seconds":3600}));
    assert_eq!(renewed["epoch"], mine["epoch"]);
    assert!(renewed["lease_until"].as_i64() > mine["lease_until"].as_i64());
    assert!(renewed.get("claim_token").is_none());
    worker.write(
        &json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":parked}})
            .to_string(),
    );
    assert_eq!(
        worker.call("tasks_release", json!({"id":mine["id"]}))["state"],
        "ready"
    );
    let parked = worker.send("tools/call", req);
    assert_eq!(worker.request("ping", json!({}))["result"], json!({}));
    boss.call("tasks_complete", json!({"id":first["id"]}));
    let response = worker.read();
    assert_eq!(response["id"], parked);
    assert_eq!(response["result"]["structuredContent"]["id"], next["id"]);
    let task = Client::content(response, true);
    assert!(task.get("claim_token").is_none());
    assert_eq!(
        worker.call("tasks_complete", json!({"id":task["id"]}))["state"],
        "completed"
    );
}

#[test]
fn contended_queue_wait_yields_to_ping_and_never_leases_after_its_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("contention");
    let mut c = Client::start(&path, None, None);
    c.initialize();
    let task = c.call("tasks_create", json!({"channel":"pool","text":"ready"}));
    let writer = rusqlite::Connection::open(&path).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let id = c.send(
        "tools/call",
        json!({"name":"tasks_claim","arguments":{"ready_channel":"pool","wait_seconds":1}}),
    );
    assert_eq!(c.request("ping", json!({}))["result"], json!({}));
    let reply = c.read();
    assert_eq!(reply["id"], id);
    assert_eq!(Client::content(reply, false)["code"], "nothing_ready");
    writer.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        c.call("tasks_claim", json!({"ready_channel":"pool"}))["id"],
        task["id"]
    );
}

#[test]
fn registered_inbox_cancellation_and_timeout_survive_writer_contention() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inbox");
    let mut boss = Client::start(&path, None, None);
    boss.initialize();
    boss.call(
        "swarm_start",
        json!({"slug":"release","goal":"Interrupt an inbox"}),
    );
    boss.call(
        "swarm_add",
        json!({"name":"review","brief":"Wait for notifications"}),
    );
    let mut worker = Client::start_session(&path, None, None, Some("release/review"));
    worker.initialize();
    let pending = worker.send(
        "tools/call",
        json!({"name":"swarm_wait","arguments":{"timeout_seconds":20}}),
    );
    worker.request("ping", json!({}));
    assert_eq!(
        boss.call("swarm_status", json!({}))["workers"][0]["status"],
        "waiting"
    );
    let writer = rusqlite::Connection::open(&path).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    worker.write(
        &json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":pending}})
            .to_string(),
    );
    worker.request("ping", json!({}));
    writer.execute_batch("ROLLBACK").unwrap();
    let msg = boss.call("swarm_dm", json!({"to":"review","text":"Still unread"}));
    let until = Instant::now() + Duration::from_secs(4);
    while boss.call("swarm_status", json!({}))["workers"][0]["status"] == "waiting" {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(15));
    }
    let delivery = worker.call("swarm_wait", json!({"timeout_seconds":0}));
    assert_eq!(delivery["dms"][0]["id"], msg["id"]);
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert_eq!(
        worker.denied(
            "swarm_wait",
            json!({"timeout_seconds":0,"ack":delivery["cursor"]})
        )["code"],
        "database_busy"
    );
    writer.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        worker.call("swarm_wait", json!({"timeout_seconds":0}))["dms"][0]["id"],
        msg["id"]
    );
    let timeout = worker.send(
        "tools/call",
        json!({"name":"swarm_wait","arguments":{"timeout_seconds":1,"ack":delivery["cursor"]}}),
    );
    worker.request("ping", json!({}));
    assert_eq!(
        boss.call("swarm_status", json!({}))["workers"][0]["status"],
        "waiting"
    );
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let reply = worker.read();
    assert_eq!(reply["id"], timeout);
    assert_eq!(
        Client::content(reply, true),
        json!({"status":"still_waiting","cursor":delivery["cursor"]})
    );
    worker.request("ping", json!({}));
    writer.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn plans_pages_and_observations_resume_without_replaying_the_roster() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board");
    let mut boss = Client::start(&db, None, None);
    boss.initialize();
    boss.call(
        "swarm_start",
        json!({"slug":"release","goal":"two leaves and a join","max_active":2,"retry_budget":1}),
    );
    boss.call(
        "swarm_add",
        json!({"name":"tester","brief":"own ready work"}),
    );
    let mut worker = Client::start_session(&db, None, None, Some("release/tester"));
    worker.initialize();
    let compact = worker.call("swarm_check_in", json!({"compact":true}));
    assert_eq!(compact["actor"], "ada/tester");
    assert!(compact.get("workers").is_none());
    let mut plan = json!({"channel":"swarms/release","key":"release-v1","tasks":[
        {"key":"join","text":"Synthesize both leaves","needs":["a","b"],"no_sync":true},
        {"key":"a","text":"Analyze A","no_sync":true},
        {"key":"b","text":"Analyze B","no_sync":true}]});
    let made = boss.call("tasks_plan", plan.clone());
    let original_plan = plan.clone();
    assert_eq!(boss.call("tasks_plan", plan.clone()), made);
    plan["tasks"][1]["text"] = json!("changed");
    assert_eq!(boss.denied("tasks_plan", plan)["code"], "duplicate_id");
    let a = made["tasks"][1]["id"].clone();
    boss.call("tasks_block", json!({"id":a,"reason":"awaiting fixture"}));
    let b = made["tasks"][2]["id"].clone();
    boss.call("tasks_block", json!({"id":b,"reason":"awaiting input"}));
    assert_eq!(made["tasks"][0]["needs"], json!([a, b]));
    let obs = boss.call("swarm_observe", json!({"limit":1}));
    assert_eq!(obs["tasks"], json!({"waiting":1,"blocked":2}));
    assert_eq!(obs["available_capacity"], 1);
    assert_eq!(obs["policy"]["retry_budget"], 1);
    assert_eq!(obs["actions"][0]["blocked_dependents"], 1);
    let page = boss.call(
        "swarm_observe",
        json!({"limit":1,"action_after":obs["next_action_after"],"after":obs["cursor"]}),
    );
    assert_ne!(page["actions"][0]["task_id"], obs["actions"][0]["task_id"]);
    assert!(page["changes"].as_array().unwrap().is_empty());
    assert_eq!(page["cursor"], obs["cursor"]);
    let first = worker.call("tasks_list", json!({"channel":"swarms/release","limit":2}));
    let last = worker.call(
        "tasks_list",
        json!({"channel":"swarms/release","limit":2,"after":first["next_after"]}),
    );
    assert_eq!(last["tasks"].as_array().unwrap().len(), 1);
    assert!(last["next_after"].is_null());
    assert_ne!(first["cursor"], first["next_after"]);
    let mut cursor = json!(0);
    loop {
        let delta = boss.call("swarm_observe", json!({"after":cursor,"limit":2}));
        let rows = delta["changes"].as_array().unwrap();
        assert!(rows.iter().all(|c| c["cursor"].as_i64() > cursor.as_i64()));
        assert_eq!(delta["cursor"], rows.last().unwrap()["cursor"]);
        cursor = delta["cursor"].clone();
        if delta["has_more"] == false {
            break;
        }
    }
    assert_eq!(cursor, obs["cursor"]);
    let pending = worker.send(
        "tools/call",
        json!({"name":"swarm_wait","arguments":{"timeout_seconds":5}}),
    );
    worker.request("ping", json!({}));
    worker.write(
        &json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":pending}})
            .to_string(),
    );
    assert_eq!(
        worker.call("swarm_status", json!({}))["workers"][0]["status"],
        "working"
    );
    worker.send("tools/call",json!({"name":"tasks_claim","arguments":{"ready_channel":"swarms/release","wait_seconds":5}}));
    boss.call("swarm_stop", json!({}));
    assert_eq!(
        Client::content(worker.read(), false)["code"],
        "swarm_stopped"
    );
    let replay = boss.call("tasks_plan", original_plan);
    assert_eq!(replay["tasks"][1]["id"], a);
    assert_eq!(replay["tasks"][1]["state"], "blocked");
    assert_eq!(
        worker.denied(
            "tasks_create",
            json!({"channel":"elsewhere","text":"no new lease","claim":true})
        )["code"],
        "swarm_stopped"
    );
    assert!(
        boss.call("tasks_list", json!({"channel":"elsewhere","state":"all"}))["tasks"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn typed_results_and_evidence_inputs_preserve_the_legacy_text_protocol() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board");
    let mut modern = Client::start(&db, None, None);
    modern.initialize();
    let response = modern.request("tools/list", json!({}));
    let defs = response["result"]["tools"].as_array().unwrap();
    for tool in ["swarm_post", "board_post", "board_reply"] {
        let post = defs.iter().find(|t| t["name"] == tool).unwrap();
        let cases = post["inputSchema"]["allOf"].as_array().unwrap();
        for name in ["handoff@1", "verification@1"] {
            let shape = cases
                .iter()
                .find(|s| s["if"]["properties"]["schema"]["const"] == name)
                .unwrap();
            assert!(
                shape["then"]["properties"]["schema_data"]["properties"]["revision"].is_object()
            );
        }
    }
    let mut old = Client::start(&db, None, None);
    old.request("initialize", json!({"protocolVersion":"2025-03-26"}));
    let reply = old.request("tools/call", json!({"name":"roles_list","arguments":{}}));
    assert!(reply["result"].get("structuredContent").is_none());
    assert!(Client::content(reply, true).is_array());
}

#[test]
fn ordinary_public_replies_record_evidence_once_on_retry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board");
    let mut boss = Client::start(&path, None, None);
    boss.initialize();
    let run = boss.call(
        "swarm_start",
        json!({"slug":"release","goal":"verify one change"}),
    );
    boss.call("swarm_add", json!({"name":"check","brief":"verify"}));
    let mut verifier = Client::start_session(&path, None, None, Some("release/check"));
    verifier.initialize();
    let args = json!({"channel":run["channel"],"text":"Review @check","idempotency_key":"mcp-public-root"});
    let root = boss.call("board_post", args.clone());
    assert_eq!(boss.call("board_post", args)["id"], root["id"]);
    let args = json!({"id":root["id"],"text":"Only 90% coverage: accept?","schema":"decision-request@1","schema_data":{"to":"ada","summary":"Accept coverage?"},"idempotency_key":"mcp-public-reply"});
    let reply = verifier.call("board_reply", args.clone());
    assert_eq!(reply["re"], root["id"]);
    assert_eq!(verifier.call("board_reply", args)["id"], reply["id"]);
    let receipt = boss.call("swarm_receipt", json!({}));
    assert_eq!(receipt["decisions"].as_array().unwrap().len(), 1);
    assert_eq!(receipt["decisions"][0]["id"], reply["id"]);
}

#[test]
fn ordinary_mcp_plan_can_repair_an_uncommitted_suffix() {
    let dir = tempfile::tempdir().unwrap();
    let mut boss = Client::start(&dir.path().join("board"), None, None);
    boss.initialize();
    let mut tasks = (0..101)
        .map(|i| json!({"key":format!("i{i}"),"text":"Investigate"}))
        .collect::<Vec<_>>();
    tasks[100]["needs"] = json!([999999]);
    boss.denied(
        "tasks_plan",
        json!({"channel":"work","key":"plan","tasks":tasks}),
    );
    let committed = boss.call("tasks_list", json!({"channel":"work","limit":110}));
    assert_eq!(committed["tasks"].as_array().unwrap().len(), 100);
    tasks[100]["needs"] = json!(["i0"]);
    let retried = boss.call(
        "tasks_plan",
        json!({"channel":"work","key":"plan","tasks":tasks}),
    );
    let retried_tasks = retried["tasks"].as_array().unwrap();
    assert_eq!(retried_tasks.len(), 101);
    for (task, original) in retried_tasks
        .iter()
        .zip(committed["tasks"].as_array().unwrap())
    {
        assert_eq!(task["id"], original["id"]);
    }
    assert_eq!(
        retried["tasks"][100]["needs"],
        json!([retried["tasks"][0]["id"]])
    );
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
    let listed = coordinator.call("swarm_list", json!({"limit":10}));
    assert_eq!(listed[0]["slug"], "release");
    let updated = coordinator.call("swarm_set_limit", json!({"max_notifications":25}));
    assert_eq!(updated["max_notifications"], 25);
    let id = run["id"].as_i64().unwrap();
    let spawn = coordinator.call(
        "swarm_add",
        json!({"run":id,"name":"checker","role":"verifier","brief":"Verify the receipt","export_local":true}),
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
fn a_restarted_stdio_waiter_recovers_a_mention_after_losing_the_delivery_response() {
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
    let message = boss.call(
        "swarm_post",
        json!({"text":"@checker verify this revision"}),
    )["message"]["id"]
        .clone();
    // The server has finished the wait; deliberately kill the connection without reading its reply.
    let deadline = Instant::now() + Duration::from_secs(5);
    while boss.call("swarm_status", json!({}))["workers"][0]["status"] == "waiting" {
        assert!(
            Instant::now() < deadline,
            "server did not finish the delivery"
        );
        thread::sleep(Duration::from_millis(15));
    }
    first.child.kill().unwrap();
    first.child.wait().unwrap();
    let mut replacement = Client::start(&path, Some("checker"), None);
    replacement.initialize();
    let result = replacement.call("swarm_wait", json!({"timeout_seconds":0}));
    assert_eq!(result["status"], "messages");
    assert_eq!(result["cursor"], message);
    assert_eq!(result["messages"][0]["id"], message);
    assert_eq!(
        replacement.call(
            "swarm_wait",
            json!({"timeout_seconds":0,"ack":result["cursor"]})
        )["status"],
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
    reader.denied(
        "tasks_plan",
        json!({"channel":"bugs","tasks":[{"text":"must not write"}]}),
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
