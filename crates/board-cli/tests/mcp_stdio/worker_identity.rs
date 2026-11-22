use super::client::Client;
use serde_json::{Value, json};
use std::{path::Path, process::Command};

fn board(db: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_board"));
    cmd.env_clear()
        .env("BOARD_USER", "ada")
        .env("BOARD_DB", db)
        .arg("--json");
    cmd
}

#[cfg(unix)]
#[test]
fn invalid_identity_defaults_never_fall_back_to_the_chat_actor() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
    let home = tempfile::tempdir().unwrap();
    for key in ["BOARD_PROFILE", "BOARD_WORKER"] {
        let out = board(&home.path().join("board.sqlite3"))
            .env(key, OsStr::from_bytes(&[0xff]))
            .args(["post", "public", "must not post"])
            .output()
            .unwrap();
        assert!(!out.status.success(), "{key}");
    }
}
fn connect(cmd: Command, mode: Option<&str>) -> Client {
    let mut client = Client::spawn(cmd, mode);
    client.initialize();
    client
}

#[test]
fn named_workers_verify_a_revision_and_resolve_a_decision_without_tasks_or_leases() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("workflow.sqlite");
    let mut chat = connect(board(&db), None);
    chat.tool(
        "swarm_start",
        json!({"channel":"release","goal":"Verify revision","slug":"audit"}),
        true,
    );
    let root = chat.tool(
        "board_post",
        json!({"channel":"code","text":"Review the Unicode fix"}),
        true,
    )["id"]
        .clone();
    for name in ["impl", "verify"] {
        chat.tool(
            "swarm_add",
            json!({"run":"audit","name":name,"thread_id":root,"brief":"Review the fix"}),
            true,
        );
    }
    let worker = |name| {
        let mut command = board(&db);
        command.args(["--worker", name]);
        connect(command, None)
    };
    let mut implementer = worker("audit/impl");
    let mut verifier = worker("audit/verify");
    assert_eq!(
        implementer.tool("worker_assignment", json!({}), true)["run"],
        "audit"
    );
    let revision = json!({"kind":"sha256","value":"a".repeat(64)});
    let h=implementer.tool("board_post",json!({"reply_to":root,"text":"Ready","schema":"handoff@1","schema_data":{"revision":revision,"summary":"Unicode fix","changes":["fields.rs"],"to":"verify"}}),true);
    let handoff = h["id"].as_i64().unwrap();
    let inbox = verifier.tool("swarm_wait", json!({"timeout_seconds":0}), true);
    assert_eq!(inbox["messages"][0]["id"], handoff);
    let check = verifier.tool("board_post",json!({"reply_to":root,"text":"Verified","schema":"verification@1","schema_data":{"handoff":handoff,"revision":revision,"summary":"Passed","checks":[{"name":"unit","status":"pass","command":"cargo test"}]}}),true);
    let d=verifier.tool("board_post",json!({"reply_to":root,"text":"Decision needed","schema":"decision-request@1","schema_data":{"to":"ada","summary":"Accept this change?","handoff":handoff}}),true);
    let request = d["id"].as_i64().unwrap();
    let resolution = |summary| json!({"reply_to":root,"text":summary,"schema":"decision-resolution@1","schema_data":{"request":request,"summary":summary}});
    implementer.tool("board_post", resolution("Self-approval"), false);
    chat.tool("board_post", resolution("Accepted"), true);
    let thread = chat.tool("board_read", json!({"target":root}), true);
    let verified = &thread["replies"][1];
    assert_eq!(verified["id"], check["id"]);
    assert_eq!(verified["actor"], "ada/verify");
    assert_eq!(verified["blocks"][0]["data"]["revision"], revision);
    let overview = chat.tool("swarm_observe", json!({"run":"audit"}), true);
    assert!(overview["actions"].as_array().unwrap().is_empty());
    assert!(overview["tasks"].as_object().unwrap().is_empty());
    chat.tool("swarm_stop", json!({"run":"audit"}), true);
    let create = board(&db)
        .args([
            "--worker",
            "audit/impl",
            "tasks",
            "create",
            "other",
            "Cannot acquire",
            "--claim",
        ])
        .output()
        .unwrap();
    assert!(!create.status.success());
    assert!(String::from_utf8_lossy(&create.stderr).contains("swarm_stopped"));
}

#[test]
fn dedicated_worker_reads_only_its_pinned_assignment_without_a_local_export() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("portable.sqlite");
    let board_db = board_local::Board::open(&db).unwrap();
    let library = board_db.role_library_as("ada");
    {
        let files = tempfile::tempdir().unwrap();
        let skill = files.path().join("release-check");
        std::fs::create_dir_all(skill.join("references")).unwrap();
        std::fs::write(skill.join("SKILL.md"), "---\nname: release-check\ndescription: Check a release\n---\nRead references/checks.md").unwrap();
        std::fs::write(
            skill.join("references/checks.md"),
            "Check the pinned revision.",
        )
        .unwrap();
        library.skill_import(&skill).unwrap();
        library
            .role_attach_skill("verifier", "release-check")
            .unwrap();
    }
    let mut boss = connect(board(&db), Some("--full"));
    boss.tool(
        "swarm_start",
        json!({"slug":"sample","channel":"shipping","goal":"Ship a portable brief"}),
        true,
    );
    let register = |name, role| json!({"run":"sample","name":name,"role":role,"brief":"Check only your slice"});
    let first = boss.tool("swarm_add", register("check", "verifier"), true);
    assert_eq!(first["selector"], "sample/check");
    assert!(first.get("skills_dir").is_none());
    boss.tool("swarm_add", register("build", "implementer"), true);
    assert!(!tmp.path().join("portable.sqlite.data").exists());
    assert_eq!(
        boss.request(
            "tools/call",
            json!({"name":"worker_assignment","arguments":{}})
        )["error"]["code"],
        -32602
    );
    let mut cmd = board(&db);
    cmd.args(["--worker", "sample/check"]);
    let mut worker = connect(cmd, Some("--full"));
    let assignment = worker.tool("worker_assignment", json!({}), true);
    assert_eq!(assignment["goal"], "Ship a portable brief");
    assert_eq!(assignment["worker"]["brief"], "Check only your slice");
    assert_eq!(assignment["skills"][0]["files"], json!(["SKILL.md"]));
    let edit = board(&db)
        .args(["roles", "edit", "verifier", "--prompt", "New role version"])
        .output()
        .unwrap();
    assert!(
        edit.status.success(),
        "{}",
        String::from_utf8_lossy(&edit.stderr)
    );
    assert_eq!(
        worker.tool("worker_assignment", json!({}), true),
        assignment
    );
    library.delete_skill_bundle("release-check", true).unwrap();
    let file = worker.tool(
        "worker_assignment",
        json!({"skill":"release-check","path":"references/checks.md"}),
        true,
    );
    let via_cli = board(&db)
        .args([
            "--worker",
            "sample/check",
            "swarm",
            "assignment",
            "--skill",
            "release-check",
            "--path",
            "references/checks.md",
        ])
        .output()
        .unwrap();
    assert!(via_cli.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&via_cli.stdout).unwrap(),
        file
    );
    assert_eq!(file["text"], "Check the pinned revision.");
    let read = worker.tool(
        "worker_assignment",
        json!({"skill":"board-verifier","path":"SKILL.md"}),
        true,
    );
    assert!(read["text"].as_str().unwrap().contains("board-verifier"));
    worker.tool(
        "worker_assignment",
        json!({"skill":"board-verifier","path":"../../private"}),
        false,
    );
    worker.tool(
        "worker_assignment",
        json!({"skill":"board-implementer","path":"SKILL.md"}),
        false,
    );
    worker.tool("worker_assignment", json!({"name":"build"}), false);
    worker.tool(
        "swarm_check_in",
        json!({"model":"example-model","workspace_mode":"isolated","session_ref":"test:session-1"}),
        true,
    );
    let status = boss.tool("swarm_status", json!({"run":"sample"}), true);
    assert_eq!(status["workers"][0]["session_ref"], "test:session-1");
    assert_eq!(status["workers"][0]["model"], "example-model");
    assert_eq!(status["workers"][0]["workspace_mode"], "isolated");
    assert!(status["workers"][1].get("model").is_none());
    worker.tool("swarm_check_in", json!({}), true);
    assert_eq!(
        boss.tool("swarm_status", json!({"run":"sample"}), true)["workers"][0]["model"],
        "example-model"
    );
    worker.tool("swarm_check_in", json!({"model":"contains\nbreak"}), false);
    worker.tool(
        "swarm_check_in",
        json!({"session_ref":"test:session-2"}),
        true,
    );
    let status = boss.tool("swarm_status", json!({"run":"sample"}), true);
    assert_eq!(status["workers"][0]["session_ref"], "test:session-2");
    assert!(status["workers"][0].get("model").is_none());
}

#[test]
fn workers_use_their_assignment_and_never_rebind_the_shared_chat() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("board test.sqlite3");
    let mut chat = connect(board(&db), None);
    chat.tool(
        "swarm_start",
        json!({"channel":"release","goal":"Check two roles","slug":"release","repo_root":"/workspace/release"}),
        true,
    );
    chat.tool(
        "swarm_start",
        json!({"channel":"release","goal":"Other run","slug":"other"}),
        true,
    );
    for (name, role) in [("build", "implementer"), ("check", "verifier")] {
        let spawn = chat.tool(
            "swarm_add",
            json!({"run":"release","name":name,"role":role,"brief":"Test the saved revision"}),
            true,
        );
        let actor = format!("ada/{name}");
        let selector = format!("release/{name}");
        assert_eq!(spawn["selector"], selector);
        let prefix: Vec<String> = serde_json::from_value(spawn["cli_args"].clone()).unwrap();
        // Subagents can inherit the entire coordinator environment and no child overrides.
        let scoped = || {
            let mut c = board(&db);
            c.args(&prefix)
                .env("BOARD_PROFILE", "chat")
                .env("BOARD_SWARM_RUN", "other")
                .env("BOARD_USER", "other-person");
            c
        };
        let assignment = scoped().args(["swarm", "assignment"]).output().unwrap();
        assert!(assignment.status.success());
        let assignment: Value = serde_json::from_slice(&assignment.stdout).unwrap();
        assert_eq!(assignment["repo_root"], "/workspace/release");
        assert_eq!(assignment["worker"]["actor"], actor);
        assert_eq!(assignment["worker"]["brief"], "Test the saved revision");
        let file = scoped()
            .args([
                "swarm",
                "assignment",
                "--skill",
                &format!("board-{role}"),
                "--path",
                "SKILL.md",
            ])
            .output()
            .unwrap();
        assert!(file.status.success());
        assert!(
            serde_json::from_slice::<Value>(&file.stdout).unwrap()["text"]
                .as_str()
                .unwrap()
                .contains("description:")
        );
        assert!(!tmp.path().join("board test.sqlite3.data").exists());
        let who = scoped().arg("whoami").output().unwrap();
        let who: Value = serde_json::from_slice(&who.stdout).unwrap();
        assert_eq!(who["actor"], actor);
        assert_eq!(who["worker"]["run"], "release");
        let result = scoped()
            .args(["swarm", "post", "verified independently"])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let post: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(post["message"]["actor"], actor);
        for bad in [
            &["swarm", "stop", "release"][..],
            &["swarm", "post", "wrong", "--run", "other"],
            &["--as", "admin", "post", "any", "wrong"],
            &["swarm", "start", "not my decision"],
            &["roles", "edit", "verifier", "--prompt", "wrong"],
        ] {
            assert!(
                !scoped().args(bad).output().unwrap().status.success(),
                "{bad:?}"
            );
        }
        let mut own = connect(scoped(), None);
        let advertised: Vec<_> = own.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect();
        for hidden in [
            "swarm_start",
            "swarm_list",
            "swarm_add",
            "swarm_set_limit",
            "swarm_stop",
            "roles_list",
            "roles_show",
        ] {
            assert!(
                !advertised.iter().any(|n| n == hidden),
                "worker MCP should not advertise {hidden}"
            );
        }
        assert_eq!(advertised.len(), 5);
        let assignment = own.tool("worker_assignment", json!({}), true);
        assert_eq!(assignment["repo_root"], "/workspace/release");
        assert_eq!(assignment["worker"]["actor"], actor);
        assert_eq!(assignment["worker"]["name"], name);
        own.tool("swarm_observe", json!({"run":"other"}), false);
        own.tool(
            "board_post",
            json!({"channel":"release","text":"using a dedicated connection"}),
            true,
        );
    }
    assert!(
        !board(&db)
            .args(["--worker", "release/unassigned", "whoami"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let run = |args: &[&str]| {
        let out = board(&db).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<Value>(&out.stdout).unwrap()
    };
    run(&["grants", "create", "ada", "--can", "release=all"]);
    let key = run(&["credentials", "issue", "ada/build"]);
    let token = key["token"].as_str().unwrap();
    let scoped = |selector: &str| {
        board(&db)
            .env("BOARD_TOKEN", token)
            .args(["--worker", selector, "whoami"])
            .output()
            .unwrap()
    };
    assert!(scoped("release/build").status.success());
    assert!(!scoped("release/check").status.success());
    let shared = chat.tool(
        "board_post",
        json!({"channel":"release","text":"Still the coordinator"}),
        true,
    );
    assert_eq!(shared["actor"], "ada");
    assert_eq!(
        chat.tool("swarm_observe", json!({"run":"release"}), true)["status"],
        "active"
    );
}
