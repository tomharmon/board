"""Exercise a failed review, correction and independent recheck on one public task thread."""
import argparse
import base64
from contextlib import ExitStack
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile

from swarm_client import Client


def render(ui, db, run, task, directory, reply=None):
    steps = [{"type": "tap", "key": key}
             for key in ("swarms", f"swarm-run-{run}", f"swarm-task-{task}")]
    if reply:
        steps.extend([{"type": "tap", "key": "message-body"}, {"paste": reply},
                      {"type": "key", "name": "Enter"}])
    for i, step in enumerate(steps):
        step["t"] = (i + 1) * 0.25
    script, tree = directory / "steps.json", directory / "tree.json"
    script.write_text(json.dumps(steps))
    env = {k: v for k, v in os.environ.items() if not k.startswith("BOARD_")}
    env.update(BOARD_DB=str(db), BOARD_USER="human", RUNE_DIRS_ROOT=str(directory / "rune"))
    swift = Path("/usr/lib/chromium/vk_swiftshader_icd.json")
    if not Path("/dev/dri").exists() and swift.is_file() and "VK_ICD_FILENAMES" not in env:
        env.setdefault("VK_DRIVER_FILES", str(swift))
    result = subprocess.run([ui, "--script", script, "--tree", tree, "--size", "1200x900",
                             "--scale", "1", "--fps", "12", "--at", str(steps[-1]["t"] + 1.0)],
                            env=env, capture_output=True, text=True, timeout=120)
    if result.returncode:
        raise RuntimeError(f"board-ui failed: {result.stdout}\n{result.stderr}")
    output = json.loads(tree.read_text())
    assert not output["warnings"], output["warnings"]
    assert any(e.get("key") == "thread-pane" for e in output["elements"]), "task thread did not open"
    return [e.get("text", "") for e in output["elements"]]


def exercise(bindir, directory):
    db = directory / "board.sqlite3"
    with ExitStack() as cleanup:
        def connect(worker=None, *, mode=None):
            client = Client(bindir / "board", db, worker, mode=mode)
            cleanup.callback(client.close)
            return client

        boss = connect()
        title = "Review the release checksum"
        thread = boss.call("board_post", channel="release", text=title)["id"]
        run = boss.call("swarm_start", slug="review", channel="coordination", goal="Verify a published evidence file")
        task = boss.call("tasks_plan", run="review", channel="release", tasks=[{"thread_id": thread}])["tasks"][0]["id"]
        assert task == thread
        for name, role in (("author", "implementer"), ("verify", "verifier")):
            boss.call("swarm_add", run="review", name=name, role=role, brief=f"Own the {name} assignment")
        author, verifier = connect("author"), connect("verify")
        # The public review uses default sessions; optional binary evidence uses the same identities.
        author_files, verifier_files = connect("author", mode="--full"), connect("verify", mode="--full")
        assert verifier.call("worker_assignment")["worker"]["name"] == "verify"

        def submit(data, summary):
            revision = {"kind": "sha256", "value": hashlib.sha256(data).hexdigest()}
            file = author_files.call("board_file_upload", channel="release", name="release.txt",
                               content_type="text/plain", data_base64=base64.b64encode(data).decode())
            handoff = author.call("board_post", reply_to=task, text=summary,
                                  attachment_ids=[file["id"]], schema="handoff@1",
                                  schema_data={"task": task, "revision": revision, "to": "verify",
                                               "summary": summary, "changes": ["release.txt"]})
            return handoff, revision

        def check_release(handoff, revision):
            thread = verifier.call("board_read", target=task)
            evidence = next(r for r in thread["replies"] if r["id"] == handoff["id"])
            chunk = verifier_files.call("board_file_read", id=evidence["attachments"][0]["id"])
            data = base64.b64decode(chunk["data_base64"])
            assert chunk["next_offset"] is None and hashlib.sha256(data).hexdigest() == revision["value"]
            ok = data.startswith(b"Version: ")
            verdict = "Verified checksum and version header" if ok else "Missing version header in release.txt"
            result = dict(reply_to=task, text=verdict, idempotency_key=f"verify-{handoff['id']}", schema="verification@1",
                          schema_data={"handoff": handoff["id"], "revision": revision, "summary": verdict, "checks": [
                              {"name": "Version header", "status": "pass" if ok else "fail",
                               "command": "data.startswith(b'Version: ')", "output": str(ok)}]})
            check = verifier.call("board_post", **result)
            assert verifier.call("board_post", **result)["id"] == check["id"]
            return check, verdict

        handoff, revision = submit(b"Release candidate\n", "Candidate ready for review")
        first = verifier.call("swarm_wait", timeout_seconds=0)
        assert [m["id"] for m in first["messages"]] == [handoff["id"]]

        verifier.seq += 1
        verifier.send({"jsonrpc": "2.0", "id": verifier.seq, "method": "tools/call",
                       "params": {"name": "swarm_wait", "arguments": {"ack": first["cursor"], "timeout_seconds": 300}}})

        pending = verifier.seq
        verifier.rpc("ping", {})  # the wait is parked; the session still answers requests
        question = "Please check the published checksum."
        render(bindir / "board-ui", db, run["id"], task, directory, "@verify " + question)
        delivery = json.loads(verifier.lines.get(timeout=15))
        assert delivery["id"] == pending and not delivery["result"]["isError"]
        verifier.close()  # discard the delivery without acknowledging the human's reply
        verifier = connect("verify")
        replay = verifier.call("swarm_wait", timeout_seconds=0)
        notice, = replay["messages"]
        assert notice["re"] == task and notice["actor"] == "human" and question in notice["text"]
        failure, finding = check_release(handoff, revision)
        boss.close()  # no coordinator connection during the correction or recheck
        inbox = author.call("swarm_wait", timeout_seconds=0)
        assert [m["id"] for m in inbox["messages"]] == [failure["id"]]
        assert len(inbox["notifications"]) == 1
        fix, new_revision = submit(b"Version: 1\nRelease candidate\n", "Added the missing header")
        retry = verifier.call("swarm_wait", ack=replay["cursor"], timeout_seconds=0)
        assert [m["id"] for m in retry["messages"]] == [fix["id"]]
        check, verdict = check_release(fix, new_revision)
        delivered = author.call("swarm_wait", ack=inbox["cursor"], timeout_seconds=0)
        assert [m["id"] for m in delivered["messages"]] == [check["id"]]
        assert len(delivered["notifications"]) == 1
        boss = connect()
        receipts = {h["id"]: h for h in connect(mode="--full").call("swarm_receipt", run="review")["handoffs"]}
        assert receipts[handoff["id"]]["verification"]["revision"] == revision
        assert receipts[handoff["id"]]["verification"]["checks"][0]["status"] == "fail"
        assert receipts[fix["id"]]["verification"]["revision"] == new_revision
        assert receipts[fix["id"]]["status"] == "pass"
        thread = boss.call("board_read", target=task)
        posted = [r["id"] for r in thread["replies"]]
        assert posted == [handoff["id"], notice["id"], failure["id"], fix["id"], check["id"]], posted
        texts = render(bindir / "board-ui", db, run["id"], task, directory)
        for needle in (title, finding, verdict):
            assert any(needle in t for t in texts if t), (needle, texts)
        print("Desktop question replayed after disconnect; failed release corrected and independently "
              "verified with the coordinator offline; one public thread, no duplicate checks or signals.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, default=Path(__file__).resolve().parents[1] / "target/debug")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="board-handoff-") as tmp:
        exercise(args.bin_dir.resolve(), Path(tmp))
