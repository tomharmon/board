"""Prove a real desktop reply, disconnected MCP inbox, and revision-bound verification interoperate."""
import argparse
import base64
from contextlib import ExitStack
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

from swarm_pool import Client


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
        def connect(worker=None):
            client = Client(bindir / "board", db, worker)
            cleanup.callback(client.close)
            return client

        boss = connect()
        run = boss.call("swarm_start", slug="pool", goal="Verify a published evidence file")
        title = "Review the release checksum"
        task = boss.call("tasks_plan", channel=run["channel"], tasks=[{"text": title}])["tasks"][0]["id"]
        for name in ("author", "verify"):
            boss.call("swarm_add", run="pool", name=name, brief=f"Own the {name} assignment")
        author, verifier = connect("author"), connect("verify")
        assert verifier.call("worker_assignment")["worker"]["name"] == "verify"
        data = b"Release candidate: checksum fixture\n"
        revision = {"kind": "sha256", "value": hashlib.sha256(data).hexdigest()}
        file = author.call("board_file_upload", channel=run["channel"], name="release.txt",
                           content_type="text/plain", data_base64=base64.b64encode(data).decode())
        handoff = author.call("board_reply", id=task, text="Candidate ready for review",
                              attachment_ids=[file["id"]], schema="handoff@1",
                              schema_data={"task": task, "revision": revision, "to": "verify",
                                           "summary": "Release candidate", "changes": ["release.txt"]})
        first = verifier.call("swarm_wait", timeout_seconds=0)
        assert [m["id"] for m in first["messages"]] == [handoff["id"]]

        verifier.seq += 1
        verifier.send({"jsonrpc": "2.0", "id": verifier.seq, "method": "tools/call",
                       "params": {"name": "swarm_wait", "arguments": {"ack": first["cursor"], "timeout_seconds": 300}}})

        def wait_for_status(expected):
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                workers = boss.call("swarm_status", run="pool")["workers"]
                if next(w for w in workers if w["name"] == "verify")["status"] == expected:
                    return
                time.sleep(0.05)
            raise AssertionError(f"verifier did not become {expected}")

        wait_for_status("waiting")
        question = "Please check the published checksum."
        render(bindir / "board-ui", db, run["id"], task, directory, "@verify " + question)
        wait_for_status("working")
        verifier.close()  # discard the response; do not acknowledge the human's reply
        verifier = connect("verify")
        replay = verifier.call("swarm_wait", timeout_seconds=0)
        notice, = replay["messages"]
        assert notice["re"] == task and notice["actor"] == "human" and question in notice["text"]
        thread = verifier.call("board_read", target=task)
        evidence = next(r for r in thread["replies"] if r["id"] == handoff["id"])
        chunk = verifier.call("board_file_read", id=evidence["attachments"][0]["id"])
        assert chunk["next_offset"] is None
        assert hashlib.sha256(base64.b64decode(chunk["data_base64"])).hexdigest() == revision["value"]
        verdict = "Verified the published checksum"
        result = dict(id=task, text=verdict, idempotency_key="verify-release", schema="verification@1",
                      schema_data={"handoff": handoff["id"], "revision": revision, "summary": verdict, "checks": [
                          {"name": "Published bytes", "status": "pass", "command": "hashlib.sha256(decoded_file)"}]})
        check = verifier.call("board_reply", **result)
        assert verifier.call("board_reply", **result)["id"] == check["id"]
        assert verifier.call("swarm_wait", ack=replay["cursor"], timeout_seconds=0)["status"] == "still_waiting"
        author_inbox = author.call("swarm_wait", timeout_seconds=0)
        assert [m["id"] for m in author_inbox["messages"]] == [check["id"]]
        assert len(author_inbox["notifications"]) == 1
        receipt, = boss.call("swarm_receipt", run="pool")["handoffs"]
        assert receipt["status"] == "pass" and receipt["verification_id"] == check["id"]
        assert receipt["verification"]["revision"] == revision
        texts = render(bindir / "board-ui", db, run["id"], task, directory)
        for needle in (title, question, verdict):
            assert any(needle in t for t in texts if t), (needle, texts)
        print("Desktop reply replayed after MCP disconnect; attachment hash verified; "
              "one verification and one author notification on retry; original desktop thread confirmed.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, default=Path(__file__).resolve().parents[1] / "target/debug")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="board-handoff-") as tmp:
        exercise(args.bin_dir.resolve(), Path(tmp))
