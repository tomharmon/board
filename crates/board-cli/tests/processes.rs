use serde_json::Value;
use std::{
    path::Path,
    process::{Command, Output},
    sync::{Arc, Barrier},
    thread,
};

fn invoke(db: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_board"))
        .env("BOARD_DB", db)
        .env("BOARD_USER", "ada")
        .env_remove("BOARD_PROFILE")
        .env_remove("BOARD_CLAIM_EPOCH")
        .env_remove("BOARD_CLAIM_TOKEN")
        .arg("--json")
        .args(args)
        .output()
        .unwrap()
}

fn ok(db: &Path, args: &[&str]) -> Value {
    let o = invoke(db, args);
    assert!(
        o.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    serde_json::from_slice(&o.stdout).unwrap()
}

fn err(db: &Path, args: &[&str], status: i32, code: &str) {
    let o = invoke(db, args);
    assert_eq!(
        o.status.code(),
        Some(status),
        "{args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(o.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&o.stderr).unwrap()["code"],
        code
    );
}

#[test]
fn cli_retry_wait_resume_and_online_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("board.db");
    let post = ["post", "ops/shift", "Shift begins", "--id", "handoff"];
    let first = ok(&db, &post)["message"]["id"].to_string();
    assert_eq!(first, ok(&db, &post)["message"]["id"].to_string());
    assert_eq!(
        ok(&db, &["post", "ops/shift", "Recomputed", "--id", "handoff"])["message"]["text"],
        "Shift begins"
    );
    assert_eq!(
        ok(&db, &["posted", "handoff"])["message"]["id"].to_string(),
        first
    );
    err(
        &db,
        &["post", "another/channel", "Different", "--id", "handoff"],
        1,
        "duplicate_id",
    );
    ok(&db, &["react", &first, "eyes"]);
    let reaction = ok(&db, &["wait", &first, "--timeout", "0"]);
    assert_eq!(reaction["event"]["type"], "reaction.added");
    let reply = ok(
        &db,
        &[
            "--as",
            "night",
            "reply",
            &first,
            "I have the shift",
            "--after",
            &first,
        ],
    );
    let id = reply["message"]["id"].to_string();
    ok(&db, &["post", "elsewhere", "Noise"]);
    let event = ok(
        &db,
        &[
            "wait",
            &first,
            "--after",
            &reaction["event"]["cursor"].to_string(),
            "--timeout",
            "0",
        ],
    );
    assert_eq!(event["event"]["message"]["id"], reply["message"]["id"]);
    err(&db, &["wait", &id, "--timeout", "0"], 1, "timeout");
    err(
        &db,
        &["reply", &first, "Too late", "--after", &first],
        1,
        "newer_message",
    );
    let next = ok(
        &db,
        &["read", "ops/", "--flat", "--limit", "1", "--after", "0"],
    );
    let rest = ok(
        &db,
        &[
            "read",
            "ops/",
            "--flat",
            "--limit",
            "1",
            "--after",
            &next["cursor"].to_string(),
        ],
    );
    assert_eq!(rest["messages"][0]["id"], reply["message"]["id"]);
    let backup = tmp.path().join("archive.db");
    assert!(ok(&db, &["read", &first]).get("schedules").is_none());
    ok(&db, &["backup", backup.to_str().unwrap()]);
    ok(&db, &["delete", &id]);
    assert!(
        !ok(&backup, &["read", &first])["replies"][0]["deleted"]
            .as_bool()
            .unwrap()
    );
    assert!(
        ok(&db, &["read", &first])["replies"][0]["deleted"]
            .as_bool()
            .unwrap()
    );
    assert!(
        !invoke(&db, &["backup", backup.to_str().unwrap()])
            .status
            .success()
    );
    err(&db, &["read", "--bogus"], 2, "invalid_arguments");
}

#[test]
fn explicit_sync_is_atomic_and_a_join_preserves_distinct_local_and_external_roots() {
    use board_local::{Board, sources::Direction};
    use serde_json::json;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("board.db");
    for name in ["desk", "archive"] {
        ok(&path, &["sources", "add", name, "local", name, "work"]);
        ok(&path, &["sources", "publishing", name, "on"]);
    }
    ok(
        &path,
        &["sources", "configure", "desk", "--mirror-roots", "true"],
    );
    let before = Board::open(&path).unwrap().cursor().unwrap();
    assert!(
        !invoke(
            &path,
            &[
                "post",
                "work",
                "roll back",
                "--sync",
                "archive",
                "--sync",
                "unknown:target"
            ]
        )
        .status
        .success()
    );
    assert_eq!(Board::open(&path).unwrap().cursor().unwrap(), before);
    let args = [
        "post",
        "work",
        "join this conversation",
        "--sync",
        "archive",
        "--sync",
        "desk:T",
        "--sync",
        "desk:T",
        "--id",
        "join-once",
    ];
    let root = ok(&path, &args)["message"]["id"].as_i64().unwrap();
    let head = Board::open(&path).unwrap().cursor().unwrap();
    assert_eq!(ok(&path, &args)["message"]["id"], root);
    let db = Board::open(&path).unwrap();
    assert_eq!(db.cursor().unwrap(), head);
    assert!(
        db.inbox("ada", "desk", Direction::Out, 0, 50)
            .unwrap()
            .is_empty(),
        "joining an existing thread never publishes the local root"
    );
    let archive = db.inbox("ada", "archive", Direction::Out, 0, 50).unwrap();
    assert_eq!(archive.len(), 1);
    assert_eq!(archive[0].payload["event"], "thread.start");
    assert!(
        db.source_copies("ada", "desk", root).unwrap().is_empty(),
        "queueing a write cannot be evidence of delivery"
    );
    let import = |key, external, parent: Option<&str>| {
        serde_json::from_value(json!({
        "key":key, "import":{"thread":"T","external":external,"parent":parent,"author":"sam",
        "url":format!("https://example.test/conversation/{external}"),"change":{"op":"post","text":external}}
    })).unwrap()
    };
    let incoming = db
        .import_source("ada", "desk", &import("one", "original", None))
        .unwrap();
    let reply = db
        .import_source("ada", "desk", &import("two", "answer", Some("original")))
        .unwrap();
    assert_eq!(
        (incoming.re, reply.re, reply.reply_to),
        (Some(root), Some(root), Some(incoming.id))
    );
    assert!(
        db.import_source("ada", "desk", &import("three", "different-root", None))
            .is_err()
    );
    let thread = ok(&path, &["read", "https://example.test/conversation/answer"]);
    assert_eq!(thread["message"]["id"], root);
    assert_eq!(thread["replies"][1]["id"], reply.id);
    // A late join does not backdate eligibility, even on an explicit backfill.
    let id = root.to_string();
    let request = ["sync", "join-thread", "desk", "later", &id];
    assert_eq!(ok(&path, &request)["root"], root);
    let join_at = db.cursor().unwrap();
    assert_eq!(ok(&path, &request)["root"], root);
    assert_eq!(db.cursor().unwrap(), join_at);
    ok(&path, &["edit", &root.to_string(), "keep this root here"]);
    assert_eq!(
        ok(&path, &["sync", "backfill", "desk", "later"])["queued"],
        0
    );
    assert!(
        db.inbox("ada", "desk", Direction::Out, 0, 50)
            .unwrap()
            .is_empty()
    );
    let after =
        ok(&path, &["reply", &root.to_string(), "post-join reply"])["message"]["id"].clone();
    let jobs = db.inbox("ada", "desk", Direction::Out, 0, 50).unwrap();
    assert_eq!(jobs.len(), 2);
    assert!(
        jobs.iter()
            .all(|j| j.payload["message"] == after && j.payload["event"] == "message.posted")
    );
    assert!(jobs.iter().any(|j| j.thread == "later") && jobs.iter().any(|j| j.thread == "T"));
    let conflict = invoke(
        &path,
        &["post", "work", "not a second root", "--sync", "desk:T"],
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&conflict.stderr).unwrap()["code"],
        "already_linked"
    );
    err(
        &path,
        &["read", &root.to_string(), "--by", "sam"],
        2,
        "invalid_request",
    );
}

#[test]
fn waits_resume_the_same_identity_and_filters_and_skip_bookkeeping_unless_requested() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("board with spaces.db");
    ok(&path, &["channel", "unused"]);
    let raw = ok(&path, &["wait", "--all", "--after", "0", "--timeout", "0"]);
    assert_eq!(raw["event"]["type"], "channel.created");
    err(
        &path,
        &["wait", "--after", "0", "--timeout", "0"],
        1,
        "timeout",
    );
    ok(&path, &["--as", "Editor", "post", "work", "one item"]);
    ok(&path, &["--as", "EDITOR", "post", "work", "two items"]);
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_board"))
            .env("BOARD_USER", "ada")
            .env_remove("BOARD_DB")
            .env_remove("BOARD_PROFILE")
            .args(args)
            .output()
            .unwrap()
    };
    let stdout = run(&[
        "--db",
        path.to_str().unwrap(),
        "--as",
        "eDitor",
        "wait",
        "work",
        "--by",
        "me",
        "--text",
        "item",
        "--timeout",
        "0",
        "--after=0",
    ]);
    assert!(
        stdout.status.success(),
        "{}",
        String::from_utf8_lossy(&stdout.stderr)
    );
    let text = String::from_utf8(stdout.stdout).unwrap();
    assert!(text.contains("one item") && text.contains("ada/editor"));
    let argv = shlex::split(text.lines().last().unwrap().strip_prefix("next: ").unwrap()).unwrap();
    assert_eq!(argv[0], "board");
    let next = run(&argv[1..].iter().map(String::as_str).collect::<Vec<_>>());
    assert!(next.status.success());
    assert!(
        String::from_utf8(next.stdout)
            .unwrap()
            .contains("two items")
    );
}

#[test]
fn clock_sends_are_neither_advertised_nor_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board.db");
    let index = ok(&db, &["commands"]);
    let commands = index["commands"].as_array().unwrap();
    for command in ["schedule", "schedules", "worker"] {
        assert!(!commands.iter().any(|entry| entry["name"] == command));
        err(&db, &[command], 2, "invalid_arguments");
    }
    err(
        &db,
        &["post", "ops", "not later", "--in", "1h"],
        2,
        "invalid_arguments",
    );
    err(
        &db,
        &["reply", "1", "not later", "--every", "1h"],
        2,
        "invalid_arguments",
    );
    let discovery = ok(&db, &["discovery"]);
    assert!(
        !discovery["features"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "schedules")
    );
    assert_eq!(
        ok(&db, &["migrate", "export-schedules"])["schedules"],
        serde_json::json!([])
    );
}

#[test]
fn direct_attachments_are_preflighted_ordered_and_not_reuploaded_on_retry() {
    use board_local::{Board, MAX_ATTACHMENT_BYTES};
    use std::fs;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("files.db");
    let big = dir.path().join("large.txt");
    let small = dir.path().join("small.txt");
    let bytes = vec![0x41; MAX_ATTACHMENT_BYTES + 1];
    fs::write(&big, &bytes).unwrap();
    fs::write(&small, b"small").unwrap();
    let (large, small) = (big.to_str().unwrap(), small.to_str().unwrap());
    let board = Board::open(&db).unwrap();
    let head = board.cursor().unwrap();
    let missing = dir.path().join("absent");
    assert!(
        !invoke(
            &db,
            &[
                "post",
                "files",
                "bad",
                "--attach",
                large,
                "--attach",
                missing.to_str().unwrap()
            ]
        )
        .status
        .success()
    );
    assert_eq!(
        board.cursor().unwrap(),
        head,
        "all inputs must be checked before any staging"
    );
    let args = [
        "post", "files", "--attach", large, "--attach", small, "--id", "once",
    ];
    let m = ok(&db, &args)["message"].clone();
    let files = m["attachments"].as_array().unwrap();
    assert_eq!(files[0]["name"], "large.txt");
    assert_eq!(files[1]["name"], "small.txt");
    assert_eq!(
        board
            .download_as("ada", files[0]["id"].as_i64().unwrap())
            .unwrap()
            .1,
        bytes
    );
    let head = board.cursor().unwrap();
    fs::rename(&big, dir.path().join("renamed.txt")).unwrap();
    assert_eq!(ok(&db, &args)["message"], m);
    assert_eq!(board.cursor().unwrap(), head);
}

#[test]
fn independent_processes_cannot_share_or_reuse_a_claim() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("board.db");
    let id = ok(&db, &["tasks", "create", "work", "Reconcile inventory"])["id"].to_string();
    let barrier = Arc::new(Barrier::new(5));
    let attempts = thread::scope(|threads| {
        let workers: Vec<_> = (0..5)
            .map(|_| {
                let b = barrier.clone();
                let (db, id) = (&db, &id);
                threads.spawn(move || {
                    b.wait();
                    invoke(db, &["--as", "worker", "tasks", "claim", id])
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(attempts.iter().filter(|o| o.status.success()).count(), 1);
    for loser in attempts.iter().filter(|o| !o.status.success()) {
        assert_eq!(
            serde_json::from_slice::<Value>(&loser.stderr).unwrap()["code"],
            "task_claimed"
        );
    }
    let win: Value =
        serde_json::from_slice(&attempts.iter().find(|o| o.status.success()).unwrap().stdout)
            .unwrap();
    let token = win["claim_token"].as_str().unwrap();
    let epoch = win["epoch"].to_string();
    assert!(!ok(&db, &["read", &id]).to_string().contains(token));
    ok(
        &db,
        &[
            "--as", "worker", "tasks", "release", &id, "--epoch", &epoch, "--token", token,
        ],
    );
    let next = ok(&db, &["--as", "worker", "tasks", "claim", &id]);
    let fresh = next["epoch"].to_string();
    err(
        &db,
        &[
            "--as", "worker", "tasks", "complete", &id, "--epoch", &fresh, "--token", token,
        ],
        1,
        "claim_lost",
    );
    let done = ok(
        &db,
        &[
            "--as",
            "worker",
            "tasks",
            "complete",
            &id,
            "--epoch",
            &fresh,
            "--token",
            next["claim_token"].as_str().unwrap(),
            "Counted",
        ],
    );
    assert_eq!(done["state"], "completed");
    assert_eq!(
        ok(&db, &["read", &id])["message"]["task"]["result"],
        "Counted"
    );
}

#[test]
fn restarted_watcher_runs_an_event_driven_reply_without_a_worker() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("board.db");
    ok(
        &db,
        &[
            "watchers",
            "add",
            "inbox",
            "lab/",
            "--kind",
            "message.posted",
            "--label",
            "job=sample",
        ],
    );
    assert!(ok(&db, &["watchers", "next", "inbox"]).is_null());
    let sent = ok(
        &db,
        &[
            "post",
            "lab/bench",
            "Read the sample",
            "--label",
            "job=sample",
        ],
    );
    let id = sent["message"]["id"].to_string();
    let event = ok(&db, &["watchers", "next", "inbox"]);
    assert_eq!(event["message"]["text"], "Read the sample");
    assert_eq!(ok(&db, &["watchers", "next", "inbox"]), event);
    let handler = [
        env!("CARGO_BIN_EXE_board"),
        "reply",
        &id,
        "Sample reviewed",
        "--label",
        "job=sample",
    ];
    let mut run = vec!["watchers", "run", "inbox", "--once", "--"];
    run.extend(handler);
    assert_eq!(ok(&db, &run)["cursor"], event["cursor"]);
    assert!(ok(&db, &run).is_null());
    let read = ok(&db, &["read", &id]);
    assert_eq!(read["replies"].as_array().unwrap().len(), 1);
    assert_eq!(read["replies"][0]["text"], "Sample reviewed");
    assert_eq!(read["replies"][0]["actor"], "ada/inbox");
}

#[test]
#[ignore = "waits out the minimum 30-second lease against the real system clock"]
fn expired_process_cannot_finish_after_a_real_time_takeover() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("board.db");
    let id = ok(&db, &["tasks", "create", "work", "Clock test"])["id"].to_string();
    let old = ok(
        &db,
        &["--as", "runner", "tasks", "claim", &id, "--lease", "30s"],
    );
    thread::sleep(std::time::Duration::from_millis(30_100));
    // A distinct process with the SAME profile must acquire a fresh generation and secret.
    let fresh = ok(
        &db,
        &["--as", "runner", "tasks", "claim", &id, "--lease", "30s"],
    );
    assert!(fresh["epoch"].as_i64().unwrap() > old["epoch"].as_i64().unwrap());
    err(
        &db,
        &[
            "--as",
            "runner",
            "tasks",
            "complete",
            &id,
            "--epoch",
            &old["epoch"].to_string(),
            "--token",
            old["claim_token"].as_str().unwrap(),
            "stale",
        ],
        1,
        "claim_lost",
    );
    ok(
        &db,
        &[
            "--as",
            "runner",
            "tasks",
            "complete",
            &id,
            "--epoch",
            &fresh["epoch"].to_string(),
            "--token",
            fresh["claim_token"].as_str().unwrap(),
            "fresh",
        ],
    );
    assert_eq!(
        ok(&db, &["read", &id])["message"]["task"]["result"],
        "fresh"
    );
}

#[test]
fn tail_last_counts_matching_history_then_follows_live_commits() {
    use std::{
        io::{BufRead, BufReader},
        process::Stdio,
        sync::mpsc,
        time::Duration,
    };
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("board.db");
    ok(&db, &["post", "ops", "old match"]);
    let recent = ok(&db, &["post", "ops", "recent match"]);
    for _ in 0..4 {
        ok(&db, &["post", "other", "noise"]);
    }
    let mut tail = Command::new(env!("CARGO_BIN_EXE_board"))
        .env("BOARD_DB", &db)
        .args(["--json", "tail", "ops", "--last", "1"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let reader = BufReader::new(tail.stdout.take().unwrap());
    let (tx, rx) = mpsc::channel();
    let output = thread::spawn(move || {
        for line in reader.lines().take(2) {
            let _ = tx.send(serde_json::from_str::<Value>(&line.unwrap()).unwrap());
        }
    });
    let seeded = rx.recv_timeout(Duration::from_secs(5));
    let new = ok(&db, &["post", "ops", "live match"]);
    let followed = rx.recv_timeout(Duration::from_secs(5));
    tail.kill().unwrap();
    tail.wait().unwrap();
    output.join().unwrap();
    assert_eq!(seeded.unwrap()["message"]["id"], recent["message"]["id"]);
    assert_eq!(followed.unwrap()["message"]["id"], new["message"]["id"]);
}

#[test]
fn backup_includes_live_wal_blobs_and_private_consumer_state() {
    use board_local::{
        Board, Filter, PostOptions,
        automation::{add_watcher, next_event},
    };
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("active.db");
    let live = Board::open(&db).unwrap();
    add_watcher(&live, "reader", Filter::default(), Some("message.posted")).unwrap();
    let message = live
        .post(
            "ops",
            "ada",
            "Proof",
            &PostOptions {
                files: vec![("proof.bin".into(), vec![0, 128, 255])],
                ..Default::default()
            },
        )
        .unwrap();
    assert!(tmp.path().join("active.db-wal").metadata().unwrap().len() > 0);
    let path = tmp.path().join("snapshot.db");
    ok(&db, &["backup", path.to_str().unwrap()]);
    let snapshot = Board::open(path).unwrap();
    assert_eq!(snapshot.cursor().unwrap(), live.cursor().unwrap());
    assert_eq!(
        snapshot.download(message.attachments[0].id).unwrap().1,
        vec![0, 128, 255]
    );
    let delivery = next_event(&snapshot, "reader").unwrap().unwrap();
    assert_eq!(delivery.message.unwrap().id, message.id);
}
