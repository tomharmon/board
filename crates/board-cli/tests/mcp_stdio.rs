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
            json!({"name":"swarm_status","arguments":{}}),
        ),
        (
            5,
            "tools/call",
            json!({"name":"board_post","arguments":{"channel":"roundtrip","text":"scripted via MCP"}}),
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
    assert_eq!(replies.len(), 5);
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "board-mcp");
    assert_eq!(replies[1]["result"]["tools"].as_array().unwrap().len(), 21);
    assert!(!unused.exists());
    let result = |i: usize| {
        let r = &replies[i]["result"];
        assert_eq!(r["isError"], false, "{r}");
        serde_json::from_str::<Value>(r["content"][0]["text"].as_str().unwrap()).unwrap()
    };
    assert_eq!(result(2)["id"], result(3)["id"]);
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
}
