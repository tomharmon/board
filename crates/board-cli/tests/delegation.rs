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
fn launch(db: &Path, master: &str) -> (Server, String) {
    let mut server = Server(
        board(db)
            .env("BOARD_API_TOKEN", master)
            .args(["serve", "--port", "0"])
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let line = BufReader::new(server.0.stderr.take().unwrap())
        .lines()
        .next()
        .unwrap()
        .unwrap();
    let addr = line
        .strip_prefix("Board API: http://")
        .and_then(|s| s.strip_suffix("/v1"))
        .expect("announce loopback listener")
        .to_owned();
    (server, addr)
}
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
    let scope = scope
        .map(|s| format!("X-Board-Can: {s}\r\n"))
        .unwrap_or_default();
    let key = key
        .map(|k| format!("Idempotency-Key: {k}\r\n"))
        .unwrap_or_default();
    http_headers(addr, method, path, token, &format!("{scope}{key}"), data)
}
fn http_headers(
    addr: &str,
    method: &str,
    path: &str,
    token: &str,
    extra: &str,
    data: Value,
) -> (u16, Value, Option<String>) {
    let mut sock = TcpStream::connect(addr).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let body = data.to_string();
    write!(sock,"{method} /v1/{path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\n{extra}Content-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",body.len()).unwrap();
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
fn named_profile_avatar_is_committed_with_the_write_and_visible_from_rest() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("profile.db");
    let url = "https://example.invalid/profile.png";
    let named = || {
        let mut command = board(&db);
        command
            .env("BOARD_PROFILE", "Desk")
            .env("BOARD_AVATAR", url);
        command
    };
    // Merely selecting a profile, or failing a write, must not update it.
    assert!(
        named()
            .args(["channels"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let snapshot = cli(&db, &["channels"], None);
    assert!(
        !named()
            .args(["reply", "999", "missing"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(cli(&db, &["channels"], None), snapshot);
    let message = cli(
        &db,
        &["--as", "Desk", "--avatar", url, "post", "dev", "one"],
        None,
    );
    let id = message["message"]["id"].as_i64().unwrap();
    let master = "profile-test-master";
    let (_server, addr) = launch(&db, master);
    let (status, stored) = http(
        &addr,
        "GET",
        &format!("messages/{id}"),
        master,
        None,
        Value::Null,
    );
    assert_eq!(status, 200);
    assert_eq!(stored["actor"], "owner/desk");
    assert_eq!(stored["author"]["avatar"], url);
    let other = named()
        .env("BOARD_PROFILE", "Review")
        .args(["post", "dev", "two"])
        .output()
        .unwrap();
    assert!(other.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&other.stdout).unwrap()["message"]["author"]["avatar"],
        url
    );
    assert!(
        !board(&db)
            .args(["--avatar", url, "post", "dev", "no profile"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        !named()
            .env("BOARD_AVATAR", "file:///etc/passwd")
            .args(["post", "dev", "bad"])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn private_channel_creation_is_atomic_across_cli_and_http_and_never_adopts_a_public_tree() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("channels.sqlite");
    let master = "private-channel-test";
    for person in ["ada", "bea"] {
        cli(&db, &["grants", "create", person, "--can", "*=all"], None);
    }
    let token = |actor| {
        cli(&db, &["credentials", "issue", actor], None)["token"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let ada = token("ada");
    let agent = token("ada/desk");
    let bea = token("bea");
    let (_server, addr) = launch(&db, master);
    let (_, spec) = http(&addr, "GET", "openapi.json", master, None, Value::Null);
    let operation = &spec["paths"]["/channels"]["post"];
    assert_eq!(
        spec["components"]["schemas"]["NewChannel"]["properties"]["private"]["default"],
        false
    );
    assert!(operation["responses"]["201"]["headers"]["Location"].is_object());
    assert!(operation["responses"]["409"].is_object());
    // A sibling, even one matching SQL LIKE's underscore, does not overlap.
    cli(&db, &["channel", "vaultxops/existing"], None);
    let (s, body, loc) = http_with_key(
        &addr,
        "POST",
        "channels",
        &agent,
        None,
        None,
        json!({"name":"vault_ops","private":true}),
    );
    assert_eq!(s, 201, "{body}");
    assert_eq!(loc.as_deref(), Some("/v1/channels/vault_ops"));
    let grants = body["grants"].as_array().unwrap();
    assert_eq!(grants.len(), 2);
    assert_eq!(grants[0]["subject"], "ada");
    assert_eq!(grants[1]["subject"], "*");
    let id = cli(
        &db,
        &["post", "vault_ops/notes", "Hidden content"],
        Some(&ada),
    )["message"]["id"]
        .clone();
    assert_eq!(
        http(
            &addr,
            "GET",
            &format!("messages/{id}"),
            &bea,
            None,
            Value::Null
        )
        .0,
        404
    );
    assert_eq!(
        cli(&db, &["channels", "vault_ops"], Some(&bea))["channels"],
        json!([])
    );
    let made = cli(
        &db,
        &["channels", "create", "another", "--private"],
        Some(&bea),
    );
    assert_eq!(made["grants"].as_array().unwrap().len(), 2);
    assert_eq!(
        http(&addr, "GET", "channels/another", &ada, None, Value::Null).0,
        404
    );
    let before = cli(&db, &["grants", "list"], None);
    let denied = http(
        &addr,
        "POST",
        "channels",
        &ada,
        None,
        json!({"name":"vaultxops","private":true}),
    );
    assert_eq!(
        (denied.0, denied.1["code"].as_str()),
        (409, Some("channel_exists"))
    );
    let denied = http(
        &addr,
        "POST",
        "channels",
        &ada,
        None,
        json!({"name":"vault_ops","private":true}),
    );
    assert_eq!(
        (denied.0, denied.1["code"].as_str()),
        (409, Some("channel_granted"))
    );
    assert_eq!(before, cli(&db, &["grants", "list"], None));
    cli(
        &db,
        &["grants", "create", "bea", "--can", "reserved/child=read"],
        None,
    );
    assert_eq!(
        http(
            &addr,
            "POST",
            "channels",
            &ada,
            None,
            json!({"name":"reserved","private":true})
        )
        .1["code"],
        "channel_granted"
    );
    assert_eq!(
        http(
            &addr,
            "POST",
            "channels",
            &ada,
            Some("*=read,post"),
            json!({"name":"denied","private":true})
        )
        .0,
        403
    );
    assert_eq!(
        http(&addr, "GET", "channels/denied", master, None, Value::Null).0,
        404
    );
}

#[test]
fn strict_ack_over_loopback_and_cli_shares_the_proven_outcome_but_default_put_replays() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("conformance.sqlite");
    let master = "strict-ack-contract";
    cli(
        &db,
        &[
            "watchers",
            "add",
            "notes",
            "ops",
            "--kind",
            "message.posted",
        ],
        None,
    );
    let (_server, addr) = launch(&db, master);
    let (s, api) = http(&addr, "GET", "openapi.json", master, None, Value::Null);
    assert_eq!(s, 200);
    let ack_schema = &api["paths"]["/watchers/{name}/acknowledgements/{cursor}"]["put"];
    assert_eq!(ack_schema["requestBody"]["required"], false);
    assert!(ack_schema["responses"]["412"].is_object());
    assert_eq!(
        api["components"]["schemas"]["WatcherAck"]["properties"]["if_pending"]["default"],
        false
    );
    for cli_first in [false, true] {
        cli(&db, &["post", "ops", "One event"], None);
        let (status, d) = http(
            &addr,
            "POST",
            "watchers/notes/deliveries",
            master,
            None,
            json!({"lease_seconds":60}),
        );
        assert_eq!(status, 200);
        let cursor = d["event"]["cursor"].as_i64().unwrap();
        let id = cursor.to_string();
        let receipt = d["receipt"].as_str().unwrap();
        let url = format!("watchers/notes/acknowledgements/{id}");
        let tag = format!("If-Match: \"{receipt}\"\r\n");
        let strict = json!({"if_pending":true});
        let put =
            |url: &str, tag: &str, data: Value| http_headers(&addr, "PUT", url, master, tag, data);
        let ack = [
            "watchers",
            "ack",
            "notes",
            id.as_str(),
            "--receipt",
            receipt,
            "--if-pending",
        ];
        assert_eq!(put(&url, "", strict.clone()).0, 428);
        if cli_first {
            assert_eq!(cli(&db, &ack, None), cursor);
        } else {
            assert_eq!(put(&url, &tag, strict.clone()).0, 204);
        }
        let (s, result, _) = put(&url, &tag, strict.clone());
        assert_eq!(s, 412);
        assert_eq!(result["code"], "no_match");
        let retry = board(&db).args(ack).output().unwrap();
        assert_eq!(retry.status.code(), Some(1));
        assert_eq!(
            serde_json::from_slice::<Value>(&retry.stderr).unwrap()["code"],
            "no_match"
        );
        let wrong = put(&url, "If-Match: \"other\"\r\n", strict.clone());
        let missing = put(
            &format!("watchers/notes/acknowledgements/{}", cursor + 1000),
            &tag,
            strict,
        );
        assert_eq!(wrong.0, 412);
        assert_eq!(wrong.1["code"], "delivery_conflict");
        assert_eq!(wrong.1, missing.1);
        assert_eq!(put(&url, &tag, json!({})).0, 204);
        assert_eq!(cli(&db, &ack[..6], None), cursor);
    }
}

#[test]
fn read_only_sync_lookups_and_schedule_scopes_observe_live_cli_and_rest_data() {
    use board_local::{
        Board,
        access::Rules,
        automation::{Destination, Timing, schedule},
        sources::Provider,
    };
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("board.sqlite");
    let owner = Board::open(&db).unwrap();
    let mut examples = vec![];
    for (name, ch) in [
        ("hidden", "work/secret"),
        ("open", "work/public"),
        ("second", "work/public/sub"),
        ("near", "work/public-other"),
    ] {
        let url = format!("https://test.invalid/{name}");
        let root = owner
            .post(ch, "owner", "root", &Default::default())
            .unwrap();
        owner
            .create_source("owner", name, Provider::Local, name, ch)
            .unwrap();
        owner
            .attach_source("owner", name, "t", "r", root.id, Some(&url))
            .unwrap();
        let row = schedule(
            &owner,
            "owner",
            Destination::Channel(ch),
            "Later",
            Timing {
                due: chrono::Utc::now().timestamp() + 3600,
                every: None,
                times: None,
                until: None,
            },
            Default::default(),
        )
        .unwrap();
        examples.push((root.id, row.id));
    }
    owner
        .set_grant(
            "owner",
            None,
            "reader",
            &Rules::parse("work/public/=read").unwrap(),
        )
        .unwrap();
    let issued = owner
        .issue_credential("owner", "reader", None, None)
        .unwrap();
    drop(owner);
    let token = issued.token.as_str();
    let (_server, addr) = launch(&db, "local-read-only-contract");
    let get = |path: &str| http(&addr, "GET", path, token, None, Value::Null);
    let locate = cli(
        &db,
        &["sync", "locate", "https://test.invalid/open"],
        Some(token),
    );
    assert_eq!(locate["message"], examples[1].0);
    assert_eq!(
        get("links?url=https%3A%2F%2Ftest.invalid%2Fopen"),
        (200, locate)
    );
    let targets = cli(
        &db,
        &["sync", "targets", &examples[1].0.to_string()],
        Some(token),
    );
    assert_eq!(targets["targets"][0]["state"], "confirmed");
    assert_eq!(
        get(&format!("messages/{}/sync", examples[1].0)),
        (200, targets)
    );
    let hidden = get("links?url=https%3A%2F%2Ftest.invalid%2Fhidden");
    assert_eq!(hidden.0, 404);
    assert_eq!(hidden.1["code"], "message_not_found");
    assert_eq!(get("links?url=https%3A%2F%2Ftest.invalid%2Fabsent"), hidden);
    assert_eq!(get(&format!("messages/{}/sync", examples[0].0)).0, 404);
    assert!(matches!(get("sources/open/inbox").0, 403 | 404));
    let (s, page) = get("schedules?channel=work%2F&limit=1");
    assert_eq!(s, 200);
    assert_eq!(page["schedules"][0]["id"], examples[1].1);
    assert_eq!(page["next_after"], examples[1].1);
    let next = get(&format!(
        "schedules?channel=work%2F&limit=1&after={}",
        page["next_after"]
    ))
    .1;
    assert_eq!(next["schedules"][0]["id"], examples[2].1);
    assert!(next["next_after"].is_null());
    let exact = get("schedules?channel=work%2Fpublic").1;
    assert_eq!(exact["schedules"].as_array().unwrap().len(), 1);
    assert_eq!(
        cli(&db, &["schedules", "--channel", "work/public"], Some(token)),
        exact["schedules"]
    );
    assert_eq!(
        cli(&db, &["schedules", "--channel", "work/"], Some(token)),
        json!([page["schedules"][0], next["schedules"][0]])
    );
    assert_eq!(
        get("schedules?channel=work%2Fsecret").1["schedules"],
        json!([])
    );
}

#[test]
fn real_rest_installs_a_rule_cli_obeys_and_restart_keeps_source_privacy() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("board");
    let master = "local-only-contract-master";
    let (server, addr) = launch(&db, master);
    let rule = json!({"script":r#"if write.channel == "review" && write.operation == "post" { #{text:write.text+" (review)",labels:#{phase:"review"}} }"#});
    let (status, _, loc) = http_with_key(
        &addr,
        "PUT",
        "rules/review",
        master,
        None,
        None,
        rule.clone(),
    );
    assert_eq!(status, 201);
    assert_eq!(loc.as_deref(), Some("/v1/rules/review"));
    assert_eq!(
        http(
            &addr,
            "PUT",
            "rules/review",
            master,
            Some("review=read,post"),
            rule
        )
        .0,
        403
    );
    cli(
        &db,
        &["sources", "add", "mirror", "local", "other", "review"],
        None,
    );
    let (status, source) = http(
        &addr,
        "PATCH",
        "sources/mirror",
        master,
        None,
        json!({"outbound":true,"mirror_roots":true}),
    );
    assert_eq!(status, 200);
    assert_eq!(source["settings"]["mirror_roots"], true);
    assert_eq!(
        http(
            &addr,
            "PATCH",
            "sources/mirror",
            master,
            None,
            json!({"outbound":false,"mirror_files":true})
        )
        .0,
        422
    );
    assert_eq!(
        cli(&db, &["sources", "show", "mirror"], None)["outbound"],
        true
    );
    let private = cli(
        &db,
        &["post", "review", "private", "--no-sync", "--id", "once"],
        None,
    )["message"]
        .clone();
    assert_eq!(private["text"], "private (review)");
    assert_eq!(private["labels"]["phase"], "review");
    assert_eq!(private["no_sync"], true);
    assert!(
        cli(&db, &["inbox", "list", "mirror", "--outgoing"], None)
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (_s, m) = http(
        &addr,
        "POST",
        "messages",
        master,
        None,
        json!({"channel":"review","text":"public"}),
    );
    let notices = http(
        &addr,
        "GET",
        &format!("messages/{}/deliveries", m["id"]),
        master,
        None,
        Value::Null,
    )
    .1;
    assert_eq!(notices["deliveries"][0]["event"], "thread.start");
    assert!(notices["deliveries"][0].get("payload").is_none());
    drop(server);
    let (_next, addr) = launch(&db, master);
    assert_eq!(
        http(
            &addr,
            "GET",
            &format!("messages/{}", private["id"]),
            master,
            None,
            Value::Null
        )
        .1["text"],
        private["text"]
    );
    assert_eq!(
        cli(
            &db,
            &["post", "review", "private", "--no-sync", "--id", "once"],
            None
        )["message"]["id"],
        private["id"]
    );
    assert_eq!(
        http(&addr, "GET", "rules", master, None, Value::Null).1["rules"][0]["name"],
        "review"
    );
    let isolated = temp.path().join("must-not-exist");
    assert_eq!(cli(&isolated, &["conformance"], None)["ok"], true);
    assert!(!isolated.exists());
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
    let (s, msg, _) = http_headers(
        addr,
        "POST",
        "messages",
        master,
        "X-Board-Profile: Audit; avatar=\"https://example.invalid/a.png\"\r\nX-Board-Session: ignored\r\n",
        json!({"channel":"headers","text":"by a local profile"}),
    );
    assert_eq!(s, 201, "{msg}");
    assert_eq!(msg["actor"], "owner/audit");
    assert_eq!(msg["author"]["name"], "audit");
    assert_eq!(msg["author"]["avatar"], "https://example.invalid/a.png");
    assert!(msg.get("session").is_none());
    assert_eq!(
        cli(&db, &["read", &msg["id"].to_string()], None)["message"]["author"],
        msg["author"]
    );
    assert_eq!(
        http_headers(
            addr,
            "GET",
            "me",
            token,
            "X-Board-Profile: someone\r\n",
            Value::Null
        )
        .0,
        403
    );
    let (s, bare, _) = http_headers(
        addr,
        "POST",
        "messages",
        master,
        "X-Board-Profile: separate\r\nX-Board-Avatar: https://example.invalid/ignored.png\r\n",
        json!({"channel":"headers","text":"@owner/audit"}),
    );
    assert_eq!(s, 201);
    assert!(bare["author"]["avatar"].is_null());
    assert_eq!(bare["text"], "@[owner/audit]");
    assert_eq!(bare["mentions"][0]["actor"]["id"], "owner/audit");
    let (s, pair) = http(
        addr,
        "GET",
        "actors?id=owner%2Faudit&id=owner%2Fseparate&id=missing&details=true",
        master,
        None,
        Value::Null,
    );
    assert_eq!(s, 200);
    assert_eq!(pair["actors"].as_array().unwrap().len(), 2);
    let (_, hidden) = http(
        addr,
        "GET",
        "actors?id=owner%2Faudit&id=teammate%2Fhelper",
        token,
        None,
        Value::Null,
    );
    assert_eq!(hidden["actors"], json!(["teammate/helper"]));
    let many = (0..101).map(|_| "id=one").collect::<Vec<_>>().join("&");
    assert_eq!(
        http(
            addr,
            "GET",
            &format!("actors?{many}"),
            master,
            None,
            Value::Null
        )
        .0,
        400
    );
    assert_eq!(
        http(addr, "GET", "channels?prefix=pub", token, None, Value::Null).1["channels"][0]["name"],
        "public/log"
    );
    let (_, me) = http(addr, "GET", "me", master, Some("headers=read"), Value::Null);
    assert_eq!(http(addr,"PUT","profile",token,None,json!({"handle":"maintainer","name":"Maintainer","avatar":"https://example.invalid/local.png"})).0,204);
    let card = cli(
        &db,
        &[
            "actors",
            "--details",
            "--id",
            "teammate/helper",
            "--id",
            "unknown",
        ],
        Some(token),
    );
    assert_eq!(card["actors"].as_array().unwrap().len(), 1);
    assert_eq!(
        card["actors"][0]["avatar"],
        "https://example.invalid/local.png"
    );
    assert_eq!(
        http(
            addr,
            "PUT",
            "profile",
            token,
            None,
            json!({"handle":"maintainer","name":"Maintainer","avatar":"file:///tmp/private"})
        )
        .0,
        422
    );
    cli(
        &db,
        &[
            "profile",
            "--handle",
            "maintainer",
            "--name",
            "Maintainer",
            "--avatar",
            "",
        ],
        Some(token),
    );
    assert!(http(addr, "GET", "me", token, None, Value::Null).1["avatar"].is_null());
    assert_eq!(me["can"], json!({"headers":["read"]}));
    assert_eq!(me["principal"], "owner");
    assert!(me.get("session").is_none());
    let (s, replay) = http(
        addr,
        "GET",
        "events?after=0&channel=headers&kind=message.posted",
        master,
        None,
        Value::Null,
    );
    assert_eq!(s, 200, "{replay}");
    let event = &replay["events"][0];
    assert_eq!(event["actor"], "owner/audit");
    for obj in [event, &event["payload"], &event["message"]] {
        assert!(obj.get("session").is_none());
    }
    cli(
        &db,
        &["grants", "create", "lead", "--can", "ops/=manage"],
        None,
    );
    let mgr = cli(&db, &["credentials", "issue", "lead"], None)["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let (s, group) = http(
        addr,
        "POST",
        "grants",
        &mgr,
        None,
        json!({"subjects":["reviewer","spare"],"can":{"ops/review/":["read"]}}),
    );
    assert_eq!(s, 201, "{group}");
    let gpath = format!("grants/{}", group["grant"]["id"]);
    assert_eq!(http(addr, "GET", &gpath, &mgr, None, Value::Null).0, 200);
    let (s, g) = http(
        addr,
        "PATCH",
        &gpath,
        &mgr,
        None,
        json!({"add":["next"],"remove":["spare"]}),
    );
    assert_eq!(s, 200);
    assert_eq!(g["grant"]["subjects"], json!(["reviewer", "next"]));
    assert_eq!(http(addr, "GET", "grants", &mgr, None, Value::Null).0, 403);
    assert_eq!(
        http(
            addr,
            "POST",
            "credentials",
            &mgr,
            None,
            json!({"actor":"new"})
        )
        .0,
        403
    );
    assert_eq!(
        http(
            addr,
            "POST",
            "grants",
            &mgr,
            Some("ops/review/=manage;ops/review/closed/=none"),
            json!({"subject":"new","can":{"ops/review/":"read"}})
        )
        .0,
        422
    );
    assert_eq!(
        http(
            addr,
            "POST",
            "grants",
            &mgr,
            Some("ops/review/=manage;ops/review/closed/=none"),
            json!({"subject":"new","can":{"ops/review/":["read"]}})
        )
        .0,
        403
    );
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
