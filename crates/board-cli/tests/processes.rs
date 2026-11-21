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
    let reminder = ok(&db, &["schedule", &id, "Check the handoff", "--in", "30m"]);
    assert_eq!(
        ok(&db, &["read", &first])["schedules"][0]["id"],
        reminder["id"]
    );
    ok(&db, &["backup", backup.to_str().unwrap()]);
    ok(&db, &["delete", &id]);
    assert_eq!(
        ok(&db, &["read", &first])["schedules"],
        serde_json::json!([])
    );
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
fn schedules_and_unacknowledged_deliveries_outlive_processes() {
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
    let s = ok(
        &db,
        &[
            "schedule",
            "lab/bench",
            "Read the sample",
            "--in",
            "0s",
            "--label",
            "job=sample",
        ],
    );
    assert!(ok(&db, &["watchers", "next", "inbox"]).is_null());
    assert_eq!(ok(&db, &["worker", "--once"])["sent"], 1);
    assert_eq!(ok(&db, &["worker", "--once"])["sent"], 0);
    let event = ok(&db, &["watchers", "next", "inbox"]);
    assert_eq!(event["message"]["text"], "Read the sample");
    assert_eq!(ok(&db, &["watchers", "next", "inbox"]), event);
    ok(
        &db,
        &["watchers", "ack", "inbox", &event["cursor"].to_string()],
    );
    assert!(ok(&db, &["watchers", "next", "inbox"]).is_null());
    let persisted = ok(&db, &["schedules", "--all"]);
    assert_eq!(persisted[0]["id"], s["id"]);
    assert_eq!(persisted[0]["sent"], 1);
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
