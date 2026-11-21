use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};

fn board(path: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_board"));
    cmd.env("BOARD_DB", path)
        .env("BOARD_USER", "owner")
        .env_remove("BOARD_TOKEN")
        .env_remove("BOARD_PROFILE")
        .arg("--json");
    cmd
}
fn cli(path: &Path, args: &[&str], token: Option<&str>) -> Value {
    let mut cmd = board(path);
    if let Some(s) = token {
        cmd.env("BOARD_TOKEN", s);
    }
    let out = cmd.args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn http(
    addr: &str,
    method: &str,
    path: &str,
    token: &str,
    scope: Option<&str>,
    data: Value,
) -> (u16, Value) {
    let (status, value, _) = http_with_key(addr, method, path, token, scope, None, data);
    (status, value)
}
fn http_with_key(
    addr: &str,
    method: &str,
    path: &str,
    token: &str,
    scope: Option<&str>,
    key: Option<&str>,
    data: Value,
) -> (u16, Value, Option<String>) {
    let mut sock = TcpStream::connect(addr).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let body = data.to_string();
    let scope = scope
        .map(|s| format!("X-Board-Can: {s}\r\n"))
        .unwrap_or_default();
    let key = key
        .map(|k| format!("Idempotency-Key: {k}\r\n"))
        .unwrap_or_default();
    write!(sock,"{method} /v1/{path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\n{scope}{key}Content-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",body.len()).unwrap();
    let mut raw = String::new();
    sock.read_to_string(&mut raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    let location = head
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("Location"))
        .map(|(_, v)| v.trim().to_owned());
    (
        status,
        if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(body).unwrap()
        },
        location,
    )
}

#[test]
fn scoped_cli_and_actual_loopback_requests_enforce_the_same_live_grant() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("board.sqlite3");
    let g = cli(
        &db,
        &["grants", "create", "teammate", "--can", "public/=read,post"],
        None,
    );
    let id = g["grant"]["id"].to_string();
    let private = cli(&db, &["post", "private", "private text"], None)["message"]["id"].to_string();
    let issued = cli(
        &db,
        &[
            "credentials",
            "issue",
            "teammate/helper",
            "--scope",
            "public/=all",
            "--ttl",
            "600",
        ],
        None,
    );
    let token = issued["token"].as_str().unwrap();
    let key = issued["credential"]["id"].as_str().unwrap();
    let visible = cli(
        &db,
        &["post", "public/log", "from delegated CLI"],
        Some(token),
    )["message"]["id"]
        .to_string();
    assert_eq!(
        cli(&db, &["whoami"], Some(token))["actor"],
        "teammate/helper"
    );
    assert_eq!(
        cli(&db, &["channels"], Some(token))["channels"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let masking = board(&db)
        .env("BOARD_TOKEN", token)
        .args(["read", &private])
        .output()
        .unwrap();
    assert!(!masking.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&masking.stderr).unwrap()["code"],
        "message_not_found"
    );
    let master = "test-session-master-12345";
    let mut server = Server(
        board(&db)
            .env("BOARD_API_TOKEN", master)
            .args(["serve", "--port", "0"])
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut lines = BufReader::new(server.0.stderr.take().unwrap()).lines();
    let started = lines
        .next()
        .expect("server did not announce address")
        .unwrap();
    let addr = started
        .strip_prefix("Board API: http://")
        .and_then(|s| s.strip_suffix("/v1"))
        .unwrap_or_else(|| panic!("local server did not start: {started}"));
    assert_eq!(
        http(
            addr,
            "GET",
            &format!("messages/{private}"),
            token,
            None,
            Value::Null
        )
        .0,
        404
    );
    assert_eq!(
        http(
            addr,
            "GET",
            &format!("messages/{visible}"),
            token,
            None,
            Value::Null
        )
        .0,
        200
    );
    assert_eq!(http(addr, "GET", "grants", token, None, Value::Null).0, 403);
    let specs = json!([{"subject":"build","can":{"work/":["read"]}},{"subject":"ops","can":{"log":["post"]}}]);
    let body = json!({"grants":specs});
    let (status, first, loc) = http_with_key(
        addr,
        "POST",
        "grant-batches",
        master,
        None,
        Some("setup"),
        body.clone(),
    );
    assert_eq!(status, 201, "{first}");
    assert_eq!(first["created"], true);
    let batch_id = first["batch"]["id"].to_string();
    assert_eq!(loc, Some(format!("/v1/grant-batches/{batch_id}")));
    assert_eq!(
        cli(&db, &["grants", "receipt", &batch_id], None)["batch"],
        first["batch"]
    );
    let input = tmp.path().join("grants.json");
    std::fs::write(&input, specs.to_string()).unwrap();
    let again = cli(
        &db,
        &[
            "grants",
            "batch",
            input.to_str().unwrap(),
            "--idempotency-key",
            "setup",
        ],
        None,
    );
    assert_eq!(again["created"], false);
    assert_eq!(again["batch"], first["batch"]);
    assert_eq!(
        http_with_key(
            addr,
            "POST",
            "grant-batches",
            master,
            None,
            Some("setup"),
            body.clone()
        )
        .0,
        200
    );
    assert_eq!(
        http(
            addr,
            "GET",
            &format!("grant-batches/{batch_id}"),
            master,
            None,
            Value::Null
        )
        .1["batch"],
        first["batch"]
    );
    assert_eq!(
        http(addr, "POST", "grant-batches", token, None, body).0,
        403
    );
    assert_eq!(
        http(
            addr,
            "GET",
            &format!("grant-batches/{batch_id}"),
            token,
            None,
            Value::Null
        )
        .0,
        403
    );
    assert_eq!(
        http(addr, "GET", "credentials", token, None, Value::Null).0,
        403
    );
    assert_eq!(
        http(
            addr,
            "POST",
            "messages",
            token,
            Some("public/=read"),
            json!({"channel":"public/log","text":"cannot widen"})
        )
        .0,
        403
    );
    assert_eq!(
        http(
            addr,
            "POST",
            "messages",
            token,
            Some("*=all"),
            json!({"channel":"private","text":"also cannot widen"})
        )
        .0,
        403
    );
    let (status, reply) = http(
        addr,
        "POST",
        "messages",
        token,
        None,
        json!({"reply_to":visible.parse::<i64>().unwrap(),"text":"back from REST"}),
    );
    assert_eq!(status, 201, "{reply}");
    assert_eq!(reply["actor"], "teammate/helper");
    assert_eq!(
        cli(&db, &["read", &visible], None)["replies"][0]["text"],
        "back from REST"
    );
    cli(
        &db,
        &["grants", "update", &id, "teammate", "--can", "public/=read"],
        None,
    );
    assert_eq!(
        http(
            addr,
            "POST",
            "messages",
            token,
            None,
            json!({"channel":"public/log","text":"revoked post"})
        )
        .0,
        403
    );
    assert_eq!(
        http(
            addr,
            "GET",
            &format!("messages/{private}"),
            master,
            None,
            Value::Null
        )
        .0,
        200
    );
    cli(&db, &["credentials", "revoke", key], None);
    assert_eq!(
        http(addr, "GET", "channels", token, None, Value::Null).0,
        401
    );
    assert_eq!(
        http(addr, "GET", "channels", master, None, Value::Null).0,
        200
    );
}
