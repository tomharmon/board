use serde_json::Value;
use std::{
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

fn command(db: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_board"));
    c.env("BOARD_DB", db)
        .env("BOARD_USER", "ada")
        .env_remove("BOARD_PROFILE")
        .env_remove("BOARD_TOKEN")
        .env_remove("BOARD_CLAIMS_DIR")
        .env_remove("BOARD_REQUEST_TOKEN")
        .env_remove("BOARD_CLAIM_EPOCH")
        .env_remove("BOARD_CLAIM_TOKEN")
        .arg("--json")
        .args(args);
    c
}
fn ok(db: &Path, args: &[&str]) -> Value {
    output(command(db, args).output().unwrap())
}
fn output(o: Output) -> Value {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}
fn wait_for(p: &Path) {
    let until = Instant::now() + Duration::from_secs(5);
    while !p.exists() {
        assert!(Instant::now() < until, "worker did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn board() -> (tempfile::TempDir, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("board.db");
    (d, p)
}

#[test]
fn dash_prefixed_base64url_claim_proofs_are_values_not_cli_options() {
    let (_dir, db) = board();
    let proof = format!("-{}", "A".repeat(42));
    let created = ok(
        &db,
        &[
            "tasks", "create", "ops", "first", "--claim", "--token", &proof,
        ],
    );
    assert_eq!(created["claim_token"], proof);
    let id = created["id"].to_string();
    assert_eq!(
        ok(&db, &["tasks", "recover", "--token", &proof])["id"],
        created["id"]
    );
    assert_eq!(
        ok(
            &db,
            &["tasks", "release", &id, "--epoch", "1", "--token", &proof]
        )["state"],
        "ready"
    );
    let more = format!("-{}", "B".repeat(42));
    assert_eq!(
        ok(&db, &["tasks", "claim", &id, "--token", &more])["epoch"],
        2
    );
    assert_eq!(
        ok(
            &db,
            &[
                "tasks", "block", &id, "hold", "--epoch", "2", "--token", &more
            ]
        )["state"],
        "blocked"
    );
}

#[test]
fn failed_ack_after_cli_claim_commit_recovers_pending_proof_without_sharing_profiles() {
    let (d, db) = board();
    let first = ok(
        &db,
        &[
            "--as", "worker", "tasks", "create", "ops", "first", "--claim",
        ],
    )["id"]
        .to_string();
    let dir = d.path().join(".board-claims");
    let file = std::fs::read_dir(&dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    ok(&db, &["--as", "worker", "tasks", "release", &first]);
    let id = ok(&db, &["tasks", "create", "ops", "two"])["id"].to_string();
    let cache = rusqlite::Connection::open(file).unwrap();
    cache.execute_batch("CREATE TRIGGER interrupt_ack BEFORE UPDATE OF task ON proofs BEGIN SELECT RAISE(ABORT,'injected lost acknowledgement'); END;").unwrap();
    assert!(
        !command(&db, &["--as", "worker", "tasks", "claim", &id])
            .output()
            .unwrap()
            .status
            .success()
    );
    cache.execute_batch("DROP TRIGGER interrupt_ack").unwrap();
    assert_eq!(
        ok(&db, &["read", &id])["message"]["task"]["state"],
        "claimed"
    );
    assert!(
        !command(&db, &["--as", "sibling", "tasks", "complete", &id])
            .output()
            .unwrap()
            .status
            .success()
    );
    let proof = ok(&db, &["--as", "worker", "tasks", "recover", &id]);
    assert_eq!(proof["epoch"], 1);
    let done = ok(&db, &["--as", "worker", "tasks", "complete", &id, "saved"]);
    assert_eq!(done["result"], "saved");
    assert!(
        !command(&db, &["--as", "worker", "tasks", "recover", &id])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn multi_chunk_file_plan_round_trips_through_independent_cli_processes() {
    let (dir, db) = board();
    let plan = dir.path().join("plan.json");
    let mut items: Vec<_> = (0..101)
        .map(|i| serde_json::json!({"key":format!("task{i}"),"text":format!("step {i}")}))
        .collect();
    items[0]["needs"] = serde_json::json!([{"key":"task100"}]);
    std::fs::write(&plan, serde_json::to_vec(&items).unwrap()).unwrap();
    let cmd = [
        "tasks",
        "create",
        "work",
        "--file",
        plan.to_str().unwrap(),
        "--idempotency-key",
        "cross-process-plan",
    ];
    let first = ok(&db, &cmd);
    assert_eq!(first.as_array().unwrap().len(), 101);
    assert_eq!(first[0]["needs"][0], first[100]["id"]);
    let claimed = ok(
        &db,
        &[
            "--as",
            "worker",
            "tasks",
            "claim",
            &first[1]["id"].to_string(),
        ],
    );
    let again = ok(&db, &cmd);
    assert_eq!(again[1]["id"], claimed["id"]);
    assert_eq!(again[1]["state"], "claimed");
    let page = ok(
        &db,
        &[
            "tasks",
            "work",
            "--limit",
            "3",
            "--after",
            &first[1]["id"].to_string(),
        ],
    );
    assert_eq!(page.as_array().unwrap().len(), 3);
}

#[cfg(unix)]
#[test]
fn supervisor_renews_and_settles_child_exit_without_polluting_json() {
    let (_d, db) = board();
    let id = ok(&db, &["tasks", "create", "ops", "work"])["id"].to_string();
    let success = command(
        &db,
        &[
            "tasks",
            "run",
            &id,
            "--lease",
            "30s",
            "--result",
            "verified",
            "--",
            "sh",
            "-c",
            "sleep 11; printf 'child-task=%s' \"$BOARD_TASK\"",
        ],
    )
    .output()
    .unwrap();
    assert_eq!(
        String::from_utf8(success.stderr.clone()).unwrap(),
        format!("child-task={id}")
    );
    assert_eq!(output(success)["result"], "verified");
    let board = board_local::Board::open(&db).unwrap();
    assert!(
        board
            .events(0, 100)
            .unwrap()
            .iter()
            .any(|e| e.kind == "task.renewed")
    );
    let bad = ok(&db, &["tasks", "create", "ops", "failure"])["id"].to_string();
    let failure = command(&db, &["tasks", "run", &bad, "--", "sh", "-c", "exit 7"])
        .output()
        .unwrap();
    assert_eq!(failure.status.code(), Some(7));
    assert_eq!(
        serde_json::from_slice::<Value>(&failure.stdout).unwrap()["result"],
        "exited 7"
    );
    let absent = ok(&db, &["tasks", "create", "ops", "no binary"])["id"].to_string();
    assert!(
        !command(
            &db,
            &["tasks", "run", &absent, "--", "/not/a/board/program"]
        )
        .output()
        .unwrap()
        .status
        .success()
    );
    assert_eq!(
        ok(&db, &["read", &absent])["message"]["task"]["state"],
        "ready"
    );
}

#[cfg(unix)]
#[test]
fn supervisor_handles_interrupt_and_fences_children_after_another_writer_ends_the_claim() {
    let (d, db) = board();
    for interrupt in [true, false] {
        let id = ok(&db, &["tasks", "create", "ops", "work"])["id"].to_string();
        let marker = d.path().join(format!("{id}.ready"));
        let proc = command(
            &db,
            &[
                "tasks",
                "run",
                &id,
                "--lease",
                "30s",
                "--",
                "sh",
                "-c",
                ": > \"$READY\"; sleep 25",
            ],
        )
        .env("READY", &marker)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
        wait_for(&marker);
        if interrupt {
            // The supervisor, not the whole test process group, receives Ctrl-C.
            assert_eq!(unsafe { libc::kill(proc.id() as i32, libc::SIGINT) }, 0);
        } else {
            ok(&db, &["tasks", "cancel", &id, "canceled remotely"]);
        }
        let done = proc.wait_with_output().unwrap();
        assert_eq!(
            done.status.code(),
            Some(if interrupt { 130 } else { 1 }),
            "{}",
            String::from_utf8_lossy(&done.stderr)
        );
        assert_eq!(
            ok(&db, &["read", &id])["message"]["task"]["state"],
            if interrupt { "ready" } else { "canceled" }
        );
    }
}

#[cfg(unix)]
#[test]
fn explicit_watcher_handler_receives_json_renews_and_retries_without_self_triggers() {
    let (dir, db) = board();
    let capture = dir.path().join("event.json");
    ok(
        &db,
        &[
            "watchers",
            "add",
            "triage",
            "ops",
            "--kind",
            "message.posted",
        ],
    );
    let id =
        ok(&db, &["post", "ops", "ordinary input, not a command"])["message"]["id"].to_string();
    let run = |script: &str| {
        command(
            &db,
            &[
                "watchers", "run", "triage", "--once", "--lease", "1s", "--", "sh", "-c", script,
            ],
        )
        .env("BOARD_BIN", env!("CARGO_BIN_EXE_board"))
        .env("CAPTURE", &capture)
        .output()
        .unwrap()
    };
    let failed = run("exit 8");
    assert_eq!(failed.status.code(), Some(8));
    let retry: Value = serde_json::from_slice(&failed.stdout).unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    let done = output(run(
        r#"cat > "$CAPTURE"; sleep 2; "$BOARD_BIN" post ops "processed by $BOARD_PROFILE:$BOARD_TRIGGER""#,
    ));
    assert_eq!(done["cursor"].to_string(), id);
    assert_eq!(done["attempt"], 2);
    assert_eq!(done["trigger"], retry["trigger"]);
    let ev: Value = serde_json::from_slice(&std::fs::read(capture).unwrap()).unwrap();
    assert_eq!(ev["message"]["text"], "ordinary input, not a command");
    assert!(ok(&db, &["watchers", "next", "triage"]).is_null());
    let b = board_local::Board::open(db).unwrap();
    assert!(b.events(0, 100).unwrap().iter().any(|e| {
        e.actor == "ada/triage"
            && e.message.as_ref().is_some_and(|m| {
                m.text == format!("processed by triage:{}", done["trigger"].as_str().unwrap())
            })
    }));
}

#[cfg(unix)]
#[test]
fn retrying_a_handler_emits_each_post_reply_task_and_named_output_once() {
    let (_dir, db) = board();
    ok(
        &db,
        &["watchers", "add", "bot", "in", "--kind", "message.posted"],
    );
    let id = ok(&db, &["post", "in", "source"])["message"]["id"].to_string();
    let run = || {
        command(
            &db,
            &[
                "watchers",
                "run",
                "bot",
                "--once",
                "--",
                "sh",
                "-ec",
                r#"
        "$BOARD_BIN" post work "unkeyed output"
        "$BOARD_BIN" reply "$INPUT" "reply output"
        "$BOARD_BIN" tasks create work "task output"
        "$BOARD_BIN" post work "named output" --idempotency-key same-name
        if [ "$BOARD_ATTEMPT" = 1 ]; then exit 7; fi
        "#,
            ],
        )
        .env("BOARD_BIN", env!("CARGO_BIN_EXE_board"))
        .env("INPUT", &id)
        .output()
        .unwrap()
    };
    assert_eq!(run().status.code(), Some(7));
    std::thread::sleep(Duration::from_millis(1100));
    assert!(run().status.success());
    let b = board_local::Board::open(&db).unwrap();
    let events = b
        .events(0, 100)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "message.posted" && e.actor == "ada/bot")
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 4);
    assert_eq!(
        board_local::automation::list_tasks(&b, None, None)
            .unwrap()
            .len(),
        1
    );
    // A callback's explicit name does not steal an ordinary client's key.
    assert!(
        command(
            &db,
            &[
                "post",
                "work",
                "ordinary key",
                "--idempotency-key",
                "same-name"
            ]
        )
        .output()
        .unwrap()
        .status
        .success()
    );
}
