use std::{
    io::Write,
    process::{Command, Stdio},
};

use serde_json::{Value, json};

#[test]
fn installed_cli_dispatches_real_mcp_frames_to_its_selected_database_and_actor() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("selected board.sqlite3");
    let unused = tmp.path().join("not this one.sqlite3");
    let items = json!([
        {"key":"finish","text":"Finish","needs":["prepare"],"no_sync":true},
        {"key":"prepare","text":"Prepare","no_sync":true}
    ]);
    let mut server = Command::new(env!("CARGO_BIN_EXE_board"))
        .env_clear()
        .env("BOARD_DB", &unused)
        .env("BOARD_USER", "team")
        .env("BOARD_SWARM_RUN", "sample")
        .arg("--db")
        .arg(&path)
        .args(["--as", "architect", "mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = server.stdin.take().unwrap();
    for (id, method, params) in [
        (
            1,
            "initialize",
            json!({"protocolVersion":"2025-06-18","clientInfo":{"name":"board-cli-test","version":"1"},"capabilities":{}}),
        ),
        (2, "tools/list", json!({})),
        (
            3,
            "tools/call",
            json!({"name":"swarm_start","arguments":{"goal":"Verify single database","slug":"sample"}}),
        ),
        (
            4,
            "tools/call",
            json!({"name":"swarm_observe","arguments":{}}),
        ),
        (
            5,
            "tools/call",
            json!({"name":"board_post","arguments":{"channel":"roundtrip","text":"scripted via MCP"}}),
        ),
        (
            6,
            "tools/call",
            json!({"name":"tasks_plan","arguments":{"channel":"swarms/sample","key":"sample-plan","tasks":items}}),
        ),
    ] {
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        )
        .unwrap();
    }
    drop(input);
    let out = server.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let replies: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(replies.len(), 6);
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "board-mcp");
    let catalog = replies[1]["result"]["tools"].as_array().unwrap();
    for tool in ["tasks_plan", "swarm_observe", "board_post", "board_read"] {
        assert!(catalog.iter().any(|t| t["name"] == tool), "missing {tool}");
    }
    assert!(!catalog.iter().any(|t| t["name"] == "tasks_claim"));
    assert!(!unused.exists());
    let result = |i: usize| {
        let r = &replies[i]["result"];
        assert_eq!(r["isError"], false, "{r}");
        serde_json::from_str::<Value>(r["content"][0]["text"].as_str().unwrap()).unwrap()
    };
    assert_eq!(result(2)["slug"], result(3)["run"]);
    let m = result(4);
    assert_eq!(m["actor"], "team/architect");
    let read = Command::new(env!("CARGO_BIN_EXE_board"))
        .env_clear()
        .env("BOARD_USER", "team")
        .arg("--db")
        .arg(&path)
        .args(["--json", "read"])
        .arg(m["id"].to_string())
        .output()
        .unwrap();
    assert!(
        read.status.success(),
        "{}",
        String::from_utf8_lossy(&read.stderr)
    );
    let read: Value = serde_json::from_slice(&read.stdout).unwrap();
    assert_eq!(read["message"]["text"], "scripted via MCP");

    let file = tmp.path().join("plan.json");
    std::fs::write(&file, items.to_string()).unwrap();
    let replay = Command::new(env!("CARGO_BIN_EXE_board"))
        .env_clear()
        .env("BOARD_USER", "team")
        .arg("--db")
        .arg(&path)
        .args([
            "--json",
            "--as",
            "architect",
            "tasks",
            "create",
            "swarms/sample",
        ])
        .args(["--idempotency-key", "sample-plan", "--file"])
        .arg(file)
        .output()
        .unwrap();
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    let replay: Value = serde_json::from_slice(&replay.stdout).unwrap();
    assert_eq!(replay, result(5)["tasks"]);
}
