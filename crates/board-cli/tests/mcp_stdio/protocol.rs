use super::client::Client;
use board_local::{Board, access::Rules};
use serde_json::{Value, json};
use std::{path::Path, process::Command};

impl Client {
    fn full(path: &Path, profile: Option<&str>, token: Option<&str>) -> Self {
        Self::full_session(path, profile, token, None)
    }
    fn full_session(
        path: &Path,
        profile: Option<&str>,
        token: Option<&str>,
        worker: Option<&str>,
    ) -> Self {
        Self::start_mode(path, profile, token, worker, Some("--full"))
    }
    fn start_mode(
        path: &Path,
        profile: Option<&str>,
        token: Option<&str>,
        worker: Option<&str>,
        flag: Option<&str>,
    ) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_board"));
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
        Self::spawn(cmd, flag)
    }
}

#[test]
fn board_catalog_matches_what_each_connection_can_actually_call() {
    use std::collections::BTreeSet;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("board");
    let mut boss = Client::start_mode(&path, None, None, None, None);
    boss.initialize();
    boss.call(
        "swarm_start",
        json!({"channel":"release","slug":"release","goal":"Check the published catalogs"}),
    );
    boss.call(
        "swarm_add",
        json!({"name":"review","brief":"Review the catalog"}),
    );
    let plan = boss.call(
        "tasks_plan",
        json!({"channel":"release","run":"release","tasks":[{"key":"review","text":"Review catalogs"}]}),
    );
    let id = &plan["tasks"][0]["id"];
    for bound in [None, Some("release/review")] {
        let mut c = Client::start_mode(&path, None, None, bound, None);
        c.initialize();
        let list = c.request("tools/list", json!({}));
        let names: BTreeSet<_> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        let mut expected =
            BTreeSet::from(["board_post", "board_read", "swarm_observe", "swarm_wait"]);
        if bound.is_some() {
            expected.insert("worker_assignment");
        } else {
            expected.extend(["swarm_start", "swarm_add", "swarm_stop", "tasks_plan"]);
        }
        assert_eq!(names, expected, "{bound:?}");
        let before = Board::open(&path).unwrap().cursor().unwrap();
        for (hidden, args) in [
            ("swarm_post", json!({"text":"No hidden shortcut"})),
            (
                "tasks_create",
                json!({"channel":"release","text":"Blind lease","claim":true}),
            ),
            ("board_reply", json!({"id":id,"text":"Wrong tool"})),
        ] {
            assert_eq!(
                c.request("tools/call", json!({"name":hidden,"arguments":args}))["error"]["code"],
                -32602
            );
        }
        assert_eq!(c.request("tools/call", json!({"name":"tasks_claim","arguments":{"ready_channel":"release","wait_seconds":300}}))["error"]["code"], -32602);
        assert_eq!(c.request("ping", json!({}))["result"], json!({}));
        assert_eq!(Board::open(&path).unwrap().cursor().unwrap(), before);
    }
    let mut worker = Client::start_mode(&path, None, None, Some("release/review"), Some("--full"));
    worker.initialize();
    worker.denied(
        "tasks_claim",
        json!({"ready_channel":"release","wait_seconds":1}),
    );
    assert_eq!(worker.call("tasks_claim", json!({"id":id}))["id"], *id);
    assert_eq!(
        worker.call("tasks_complete", json!({"id":id,"result":"Reviewed"}))["state"],
        "completed"
    );
    assert_eq!(
        boss.call("swarm_observe", json!({}))["tasks"]["completed"],
        1
    );
}

#[test]
fn board_read_advertises_only_the_forms_it_executes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = Client::start_mode(&tmp.path().join("board"), None, None, None, None);
    c.initialize();
    let root = c.call("board_post", json!({"channel":"work","text":"first"}))["id"].clone();
    c.call(
        "swarm_start",
        json!({"thread_id":root,"slug":"review","goal":"Read this thread"}),
    );
    let listed = c.request("tools/list", json!({}));
    let schema = &listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "board_read")
        .unwrap()["inputSchema"];
    let validator = jsonschema::validator_for(schema).unwrap();
    for (valid, args) in [
        (false, json!({"target":root,"channel":"work"})),
        (false, json!({"run":"review","text":"first"})),
        (false, json!({"worker":"review"})),
        (false, json!({"run":"review","kind":"unknown"})),
        (false, json!({"target":0})),
        (false, json!({"target":root,"after":0})),
        (true, json!({"target":root,"after":root})),
        (false, json!({"after":-1})),
        (false, json!({"limit":501})),
        (false, json!({"typo":"work"})),
        (true, json!({"target":root})),
        (true, json!({"run":"review","kind":"public","after":0})),
        (true, json!({"channel":"work","text":"first","limit":1})),
        (true, json!({})),
    ] {
        assert_eq!(validator.is_valid(&args), valid, "catalog: {args}");
        let reply = c.request("tools/call", json!({"name":"board_read","arguments":args}));
        assert_eq!(
            reply["result"]["isError"], !valid,
            "dispatch: {args}: {reply}"
        );
    }
}

#[test]
fn protocol_framing_inventory_and_schema_validation() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = Client::full(&tmp.path().join("board"), None, None);
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
            // A union closes each argument form rather than the outer object.
            if t["name"] != "board_read" {
                assert_eq!(t["inputSchema"]["additionalProperties"], false);
            }
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
    let mut reader = Client::full(&path, None, Some(&token.token));
    let mut owner = Client::full(&path, None, None);
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
fn registered_inbox_cancellation_and_timeout_survive_writer_contention() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inbox");
    let mut boss = Client::full(&path, None, None);
    boss.initialize();
    boss.call(
        "swarm_start",
        json!({"channel":"release","slug":"release","goal":"Interrupt an inbox"}),
    );
    boss.call(
        "swarm_add",
        json!({"name":"review","brief":"Wait for notifications"}),
    );
    let mut worker = Client::full_session(&path, None, None, Some("release/review"));
    worker.initialize();
    let pending = worker.send(
        "tools/call",
        json!({"name":"swarm_wait","arguments":{"timeout_seconds":20}}),
    );
    worker.request("ping", json!({}));
    assert_eq!(
        boss.call("swarm_status", json!({}))["workers"][0]["status"],
        "working"
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
        "working"
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
    let mut boss = Client::full(&db, None, None);
    boss.initialize();
    boss.call(
        "swarm_start",
        json!({"channel":"release","slug":"release","goal":"two leaves and a join"}),
    );
    boss.call(
        "swarm_add",
        json!({"name":"tester","brief":"own ready work"}),
    );
    let mut worker = Client::full_session(&db, None, None, Some("release/tester"));
    worker.initialize();
    let ack = worker.call("swarm_check_in", json!({}));
    assert_eq!(
        ack,
        json!({"run":"release","worker":"tester","actor":"ada/tester","status":"working"})
    );
    worker.denied("swarm_check_in", json!({"compact":true}));
    let mut plan = json!({"run":"release","channel":"release","key":"release-v1","tasks":[
        {"key":"join","text":"Synthesize both leaves","needs":["a","b"],"no_sync":true},
        {"key":"a","text":"Analyze A","no_sync":true},
        {"key":"b","text":"Analyze B","no_sync":true}]});
    let made = boss.call("tasks_plan", plan.clone());
    let listed = boss.request("tools/list", json!({}));
    let contract = |name: &str| {
        let tool = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap();
        jsonschema::validator_for(&tool["outputSchema"]).unwrap()
    };
    let plan_output = contract("tasks_plan");
    plan_output.validate(&made).unwrap();
    assert!(!plan_output.is_valid(&json!({"key":"broken","tasks":[{"id":1}]})));
    let original_plan = plan.clone();
    assert_eq!(boss.call("tasks_plan", plan.clone()), made);
    plan["tasks"][1]["text"] = json!("changed");
    let conflict = boss.denied("tasks_plan", plan);
    assert_eq!(conflict["code"], "duplicate_id");
    plan_output.validate(&conflict).unwrap();
    let a = made["tasks"][1]["id"].clone();
    boss.call("tasks_block", json!({"id":a,"reason":"awaiting fixture"}));
    let b = made["tasks"][2]["id"].clone();
    boss.call("tasks_block", json!({"id":b,"reason":"awaiting input"}));
    assert_eq!(made["tasks"][0]["needs"], json!([a, b]));
    let obs = boss.call("swarm_observe", json!({"limit":1}));
    let observation_output = contract("swarm_observe");
    observation_output.validate(&obs).unwrap();
    assert_eq!(obs["tasks"], json!({"waiting":1,"blocked":2}));
    assert_eq!(obs["workers"]["working"], 1);
    assert!(obs.get("available_capacity").is_none() && obs.get("policy").is_none());
    assert_eq!(obs["actions"][0]["blocked_dependents"], 1);
    let focused = boss.call(
        "swarm_observe",
        json!({"explain_task":made["tasks"][0]["id"],"limit":2}),
    );
    assert_eq!(focused["task_id"], made["tasks"][0]["id"]);
    observation_output.validate(&focused).unwrap();
    assert_eq!(focused["steps"][0]["waits_on"], json!([a, b]));
    assert_eq!(focused["steps"][1]["reason"], "awaiting fixture");
    assert_eq!(focused["truncated"], true);
    assert!(focused.get("changes").is_none());
    let page = boss.call(
        "swarm_observe",
        json!({"limit":1,"action_after":obs["next_action_after"],"after":obs["cursor"]}),
    );
    assert_ne!(page["actions"][0]["task_id"], obs["actions"][0]["task_id"]);
    assert!(page["changes"].as_array().unwrap().is_empty());
    assert_eq!(page["cursor"], obs["cursor"]);
    let first = worker.call("tasks_list", json!({"channel":"release","limit":2}));
    let last = worker.call(
        "tasks_list",
        json!({"channel":"release","limit":2,"after":first["next_after"]}),
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
    boss.call("swarm_stop", json!({}));
    assert_eq!(
        worker.denied("tasks_claim", json!({"id":a}))["code"],
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
fn tool_results_include_structured_data_and_evidence_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board");
    let mut modern = Client::full(&db, None, None);
    modern.initialize();
    let response = modern.request("tools/list", json!({}));
    let defs = response["result"]["tools"].as_array().unwrap();
    for tool in ["swarm_post", "board_post"] {
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
    let mut old = Client::full(&db, None, None);
    let offered = old.request("initialize", json!({"protocolVersion":"2025-03-26"}));
    assert_eq!(offered["result"]["protocolVersion"], "2025-11-25");
    let reply = modern.request("tools/call", json!({"name":"roles_list","arguments":{}}));
    assert_eq!(
        reply["result"]["structuredContent"]["items"],
        Client::content(reply.clone(), true)
    );
}

#[test]
fn unified_posts_and_run_reads_keep_routing_and_private_history_separate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board");
    let mut boss = Client::start_mode(&path, None, None, None, None);
    boss.initialize();
    let run = boss.call(
        "swarm_start",
        json!({"channel":"release","slug":"release","goal":"Review"}),
    );
    let root = boss.call(
        "board_post",
        json!({"channel":"code","text":"Release candidate"}),
    );
    for name in ["verify", "peer"] {
        boss.call(
            "swarm_add",
            json!({"name":name,"brief":"Check a revision","thread_id":root["id"]}),
        );
    }
    let mut verifier = Client::start_mode(&path, None, None, Some("release/verify"), None);
    let mut peer = Client::start_mode(&path, None, None, Some("release/peer"), None);
    verifier.initialize();
    peer.initialize();
    let before_reply = boss.call("board_read", json!({"run":run["id"]}))["cursor"].clone();
    let reply = json!({"reply_to":root["id"],"text":"@verify Check the candidate","idempotency_key":"review"});
    let sent = boss.call("board_post", reply.clone());
    assert_eq!(sent["re"], root["id"]);
    assert_eq!(sent["notified"], json!(["verify"]));
    assert_eq!(boss.call("board_post", reply)["id"], sent["id"]);
    let thread = verifier.call("board_read", json!({"target":sent["id"]}));
    assert_eq!(thread["message"]["id"], root["id"]);
    assert_eq!(thread["replies"].as_array().unwrap().len(), 1);
    for invalid in [
        json!({"text":"missing destination"}),
        json!({"text":"ambiguous destination","channel":run["channel"],"reply_to":root["id"]}),
    ] {
        boss.denied("board_post", invalid);
    }
    let db = Board::open(&path).unwrap();
    let secret = db
        .swarm_dm("ada", "release", "verify", "Private follow-up")
        .unwrap();
    let before = (db.cursor().unwrap(), db.data_version().unwrap());
    let public = verifier.call("board_read", json!({"run":run["id"],"after":before_reply}));
    assert_eq!(public["entries"].as_array().unwrap().len(), 1);
    assert_eq!(public["entries"][0]["message"]["id"], sent["id"]);
    assert!(public["entries"][0]["dm"].is_null());
    let private_filter = json!({"run":"release","kind":"dm","worker":"verify"});
    let scoped = verifier.call("board_read", private_filter.clone());
    assert_eq!(scoped["entries"][0]["dm"]["id"], secret.id);
    assert!(
        peer.call("board_read", private_filter)["entries"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        verifier.denied("board_read", json!({"run":"other"}))["code"],
        "worker_run_mismatch"
    );
    for invalid in [
        json!({"run":"release","target":root["id"]}),
        json!({"run":"release","text":"candidate"}),
        json!({"worker":"verify"}),
        json!({"kind":"dm"}),
    ] {
        verifier.denied("board_read", invalid);
    }
    assert_eq!((db.cursor().unwrap(), db.data_version().unwrap()), before);
    let inbox = verifier.call("swarm_wait", json!({"timeout_seconds":0}));
    assert_eq!(inbox["messages"][0]["id"], sent["id"]);
    assert_eq!(inbox["dms"][0]["id"], secret.id);
}

#[test]
fn a_shared_goal_thread_can_address_one_run_without_claiming_the_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board");
    let mut boss = Client::start_mode(&db, None, None, None, None);
    boss.initialize();
    let root = boss.call(
        "board_post",
        json!({"channel":"project","text":"Release review"}),
    )["id"]
        .clone();
    for wrong in [
        json!({"goal":"Missing slug","thread_id":root}),
        json!({"goal":"Missing destination","slug":"bad"}),
        json!({"goal":"Ambiguous destination","slug":"bad","thread_id":root,"channel":"elsewhere"}),
        json!({"goal":"Retired launch limit","slug":"bad","thread_id":root,"max_active":1}),
        json!({"goal":"Retired retry budget","slug":"bad","thread_id":root,"retry_budget":1}),
    ] {
        boss.denied("swarm_start", wrong);
    }
    for (slug, name) in [("packaging", "packager"), ("docs", "editor")] {
        let run = boss.call(
            "swarm_start",
            json!({"thread_id":root,"slug":slug,"goal":"Review the release"}),
        );
        assert_eq!(run["thread_id"], root);
        assert_eq!(run["channel"], "project");
        boss.call(
            "swarm_add",
            json!({"run":slug,"name":name,"thread_id":root,"brief":"Check your part"}),
        );
    }
    let post = boss.call(
        "board_post",
        json!({"run":"packaging","reply_to":root,"text":"@packager please review"}),
    );
    assert_eq!(post["notified"], json!(["packager"]));
    let mut editor = Client::start_mode(&db, None, None, Some("docs/editor"), None);
    editor.initialize();
    assert_eq!(
        editor.call("swarm_wait", json!({"timeout_seconds":0}))["status"],
        "still_waiting"
    );
    let overview = editor.call("swarm_observe", json!({}));
    assert_eq!(overview["threads"][0]["briefed_workers"], json!(["editor"]));
    assert_eq!(overview["tasks"], json!({}));
    assert_eq!(overview["threads"][0]["state"], "thread");
}

#[test]
fn ordinary_public_replies_record_evidence_once_on_retry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board");
    let mut boss = Client::full(&path, None, None);
    boss.initialize();
    let run = boss.call(
        "swarm_start",
        json!({"channel":"release","slug":"release","goal":"verify one change"}),
    );
    boss.call("swarm_add", json!({"name":"check","brief":"verify"}));
    boss.call("swarm_add", json!({"name":"build","brief":"build"}));
    boss.call("swarm_set_limit", json!({"max_notifications":0}));
    let mut verifier = Client::full_session(&path, None, None, Some("release/check"));
    verifier.initialize();
    let args = json!({"reply_to":run["thread_id"],"text":"Review @check","idempotency_key":"mcp-public-root"});
    let root = boss.call("board_post", args.clone());
    assert_eq!(root["notified"], json!(["check"]));
    assert_eq!(boss.call("board_post", args)["id"], root["id"]);
    let args = json!({"reply_to":root["id"],"text":"@build Only 90% coverage: accept?","schema":"decision-request@1","schema_data":{"to":"ada","summary":"Accept coverage?"},"idempotency_key":"mcp-public-reply"});
    let reply = verifier.call("board_post", args.clone());
    assert_eq!(reply["re"], run["thread_id"]);
    assert_eq!(reply["reply_to"], root["id"]);
    assert_eq!(reply["notified"], json!(["coordinator"]));
    assert_eq!(reply["suppressed"], json!(["build"]));
    let replay = verifier.call("board_post", args);
    assert_eq!(replay["id"], reply["id"]);
    assert_eq!(replay["suppressed"], reply["suppressed"]);
    let receipt = boss.call("swarm_receipt", json!({}));
    assert_eq!(receipt["decisions"].as_array().unwrap().len(), 1);
    assert_eq!(receipt["decisions"][0]["id"], reply["id"]);
}

#[test]
fn plan_adopts_a_conversation_and_only_explicitly_links_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut boss = Client::full(&dir.path().join("board"), None, None);
    boss.initialize();
    boss.call(
        "swarm_start",
        json!({"slug":"release","channel":"ops","goal":"Ship"}),
    );
    let root = boss.call("board_post", json!({"channel":"bugs","text":"Slow builds"}));
    let id = &root["id"];
    let body = json!({"channel":"queue","run":"release","key":"fix","tasks":[
        {"text":"Verify","needs":["fix"]},{"key":"fix","thread_id":id}
    ]});
    boss.denied(
        "tasks_plan",
        json!({"channel":"bugs","tasks":[{"thread_id":id,"labels":{"severity":"urgent"}}]}),
    );
    let out = boss.call("tasks_plan", body.clone());
    assert_eq!(out["tasks"][0]["needs"], json!([id]));
    assert_eq!(&out["tasks"][1]["id"], id);
    assert_eq!(out["tasks"][1]["channel"], "bugs");
    assert_eq!(out["tasks"][1]["state"], "ready");
    let unrelated = boss.call(
        "tasks_plan",
        json!({"channel":"queue","tasks":[{"text":"Other work"}]}),
    );
    let overview = boss.call("swarm_observe", json!({"run":"release"}));
    let threads = overview["threads"].as_array().unwrap();
    assert!(threads.iter().any(|t| &t["id"] == id));
    assert!(threads.iter().any(|t| t["id"] == out["tasks"][0]["id"]));
    assert!(
        !threads
            .iter()
            .any(|t| t["id"] == unrelated["tasks"][0]["id"])
    );
    boss.call("swarm_stop", json!({"run":"release"}));
    assert_eq!(boss.call("tasks_plan", body), out);
    boss.denied(
        "tasks_plan",
        json!({"channel":"queue","run":"release","tasks":[{"text":"Too late"}]}),
    );
}

#[test]
fn ordinary_mcp_plan_rejects_oversized_batches_before_creating_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let mut boss = Client::full(&dir.path().join("board"), None, None);
    boss.initialize();
    let mut tasks = (0..101)
        .map(|i| json!({"key":format!("i{i}"),"text":"Investigate"}))
        .collect::<Vec<_>>();
    tasks[100]["needs"] = json!([999999]);
    boss.denied(
        "tasks_plan",
        json!({"channel":"work","key":"plan","tasks":tasks}),
    );
    let listed = boss.call("tasks_list", json!({"channel":"work","limit":110}));
    assert!(listed["tasks"].as_array().unwrap().is_empty());
    tasks.pop();
    tasks[99]["needs"] = json!(["i0"]);
    let retried = boss.call(
        "tasks_plan",
        json!({"channel":"work","key":"plan","tasks":tasks}),
    );
    assert_eq!(retried["tasks"].as_array().unwrap().len(), 100);
    assert_eq!(
        retried["tasks"][99]["needs"],
        json!([retried["tasks"][0]["id"]])
    );
}

#[test]
fn a_new_coordinator_connection_recovers_a_committed_start_by_slug() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board");
    let args = json!({"slug":"audit","goal":"Check the release","channel":"reviews"});
    {
        let mut first = Client::start_mode(&path, None, None, None, None);
        first.initialize();
        first.send("tools/call", json!({"name":"swarm_start","arguments":args}));
        // Wait for the write to finish, but retain no result or identifiers.
        first.read();
    }
    let board = Board::open(&path).unwrap();
    let head = board.cursor().unwrap();
    let mut retry = Client::start_mode(&path, None, None, None, None);
    retry.initialize();
    let run = retry.call("swarm_start", args.clone());
    assert_eq!(run["status"], "active");
    assert_eq!(board.cursor().unwrap(), head, "retry wrote to the board");
    retry.call("swarm_stop", json!({"run":"audit"}));
    let stopped = retry.call("swarm_start", args);
    assert_eq!(stopped["status"], "stopped");
    assert_eq!(stopped["thread_id"], run["thread_id"]);
}

#[test]
fn integer_run_reference_cannot_stop_a_different_numeric_slug() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = Client::full(&tmp.path().join("board"), None, None);
    c.initialize();
    let first = c.call(
        "swarm_start",
        json!({"channel":"release","goal":"first", "slug":"audit"}),
    );
    let slug = first["id"].to_string();
    c.call(
        "swarm_start",
        json!({"channel":"release","goal":"second", "slug":slug}),
    );
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
    let mut c = Client::full(&tmp.path().join("board"), None, None);
    c.initialize();
    c.call(
        "swarm_start",
        json!({"channel":"work","goal":"test", "slug":"markdown"}),
    );
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
    let mut coordinator = Client::full(&path, None, None);
    coordinator.initialize();
    let roles = coordinator.call("roles_list", json!({}));
    assert!(roles.as_array().unwrap().len() >= 6);
    assert_eq!(
        coordinator.call("roles_show", json!({"slug":"verifier"}))["slug"],
        "verifier"
    );
    let run = coordinator.call(
        "swarm_start",
        json!({"channel":"release","goal":"Check receipt recovery","slug":"release"}),
    );
    let listed = coordinator.call("swarm_list", json!({"limit":10}));
    assert_eq!(listed[0]["slug"], "release");
    assert!(listed[0]["worker_count"].is_number());
    assert!(listed[0].get("workers").is_none() && listed[0].get("activity_count").is_none());
    assert!(coordinator.call("swarm_status", json!({}))["workers"].is_array());
    let updated = coordinator.call("swarm_set_limit", json!({"max_notifications":25}));
    assert_eq!(updated["max_notifications"], 25);
    let id = run["id"].as_i64().unwrap();
    let spawn = coordinator.call(
        "swarm_add",
        json!({"run":id,"name":"checker","role":"verifier","brief":"Verify the receipt"}),
    );
    assert_eq!(spawn["worker"]["actor"], "ada/checker");
    assert_eq!(spawn["worker"]["brief"], "Verify the receipt");
    let mut checker = Client::full(&path, Some("checker"), None);
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
    checker.request("ping", json!({}));
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
    let mut boss = Client::full(&path, None, None);
    boss.initialize();
    boss.call(
        "swarm_start",
        json!({"channel":"release","slug":"release","goal":"Park and recover"}),
    );
    boss.call(
        "swarm_add",
        json!({"name":"checker","brief":"Verify offline"}),
    );
    let mut first = Client::full(&path, Some("checker"), None);
    first.initialize();
    first.send(
        "tools/call",
        json!({"name":"swarm_wait","arguments":{"timeout_seconds":30}}),
    );
    first.request("ping", json!({}));
    let message = boss.call(
        "swarm_post",
        json!({"text":"@checker verify this revision"}),
    )["message"]["id"]
        .clone();
    // Drop the delivered response without acknowledging the batch.
    let delivered = first.read();
    assert_eq!(
        delivered["result"]["structuredContent"]["messages"][0]["id"],
        message
    );
    first.child.kill().unwrap();
    first.child.wait().unwrap();
    let mut replacement = Client::full(&path, Some("checker"), None);
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
    assert!(worker.get("waiter_pid").is_none());
}

#[test]
fn messages_fenced_task_transitions_and_permission_errors_share_the_board() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("board");
    let mut owner = Client::full(&path, None, None);
    owner.initialize();
    owner.denied(
        "board_post",
        json!({"channel":"bugs","text":"obsolete","labels":{"kind":"issue"}}),
    );
    let root = owner.call(
        "board_post",
        json!({"channel":"bugs","text":"reproduction","no_sync":true}),
    );
    let reply = owner.call(
        "board_post",
        json!({"reply_to":root["id"],"text":"second step"}),
    );
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
        json!({"channel":"bugs","text":"fix receipt"}),
    );
    let id = &created["id"];
    owner.denied(
        "tasks_list",
        json!({"channel":"bugs","state":"ready","labels":{"priority":"next"}}),
    );
    assert_eq!(
        owner.call("tasks_list", json!({"channel":"bugs","state":"ready"}))["tasks"][0]["id"],
        *id
    );
    let mut worker = Client::full(&path, Some("dev"), None);
    worker.initialize();
    assert_eq!(
        worker.call("tasks_claim", json!({"id":id,"lease_seconds":30}))["id"],
        *id
    );
    let mut other = Client::full(&path, Some("dev"), None);
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
    let mut reader = Client::full(&path, None, Some(&secret.token));
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
    let db = dir.path().join("board.sqlite3");
    let out = Command::new(env!("CARGO_BIN_EXE_board"))
        .arg("mcp")
        .env_clear()
        .env("BOARD_DB", &db)
        .env("BOARD_WORKER", OsStr::from_bytes(&[0xff]))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid UTF-8"));
    assert!(out.stdout.is_empty() && !db.exists());
}
