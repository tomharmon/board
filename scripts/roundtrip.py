"""Exercise three real executables/processes against a disposable board (stdlib only)."""
import argparse
from contextlib import contextmanager
from http.client import HTTPConnection
import json
import os
from pathlib import Path
import queue
import re
import secrets
import subprocess
import tempfile
import threading
import time


@contextmanager
def child(argv, env):
    proc = subprocess.Popen(argv, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    lines = queue.Queue()
    history = []

    def drain():
        for line in proc.stdout:
            history.append(line.rstrip())
            lines.put(line.rstrip())
        lines.put(None)

    reader = threading.Thread(target=drain, daemon=True)
    reader.start()

    def until(pattern):
        end = time.monotonic() + 45
        while True:
            try:
                line = lines.get(timeout=max(0, end - time.monotonic()))
            except queue.Empty:
                line = None
            if line is None:
                raise RuntimeError(f"No {pattern!r} from {argv[0]}: {history!r}")
            match = re.search(pattern, line)
            if match:
                return match

    try:
        yield proc, until
    finally:
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
        proc.wait()
        reader.join()
        proc.stdout.close()


def exercise(path, bindir):
    cli, ui = bindir / "board", bindir / "board-ui"
    env = {**os.environ, "BOARD_DB": str(path / "board.sqlite3"), "BOARD_USER": "ada",
           "BOARD_API_TOKEN": secrets.token_hex(24), "RUNE_DIRS_ROOT": str(path / "rune")}
    for key in ("BOARD_PROFILE", "BOARD_TOKEN", "BOARD_CAN", "BOARD_NAME", "BOARD_CLAIMS_DIR"):
        env.pop(key, None)
    swift = Path("/usr/lib/chromium/vk_swiftshader_icd.json")
    if not Path("/dev/dri").exists() and swift.is_file() and "VK_ICD_FILENAMES" not in env:
        env.setdefault("VK_DRIVER_FILES", str(swift))

    def command(*args):
        return json.loads(subprocess.check_output([cli, "--json", *args], env=env, timeout=15))

    @contextmanager
    def server():
        with child([cli, "--as", "desk", "serve", "--port", "0"], env) as (_, until):
            port = int(until(r"Board API: http://127\.0\.0\.1:(\d+)/v1").group(1))

            def request(method, route, body=None, key=None):
                conn = HTTPConnection("127.0.0.1", port, timeout=10)
                headers = {"Authorization": "Bearer " + env["BOARD_API_TOKEN"]}
                if key:
                    headers["Idempotency-Key"] = key
                if body is not None:
                    headers["Content-Type"] = "application/json"
                try:
                    conn.request(method, "/v1" + route, None if body is None else json.dumps(body), headers)
                    res = conn.getresponse()
                    return res.status, res.getheader("Location"), json.loads(res.read())
                finally:
                    conn.close()

            yield request

    root = command("post", "studio/build", "Ready for the local review.", "--id", "handoff")["message"]
    assert command("post", "studio/build", root["text"], "--id", "handoff")["message"]["id"] == root["id"]
    print(f"CLI created #{root['id']} without a server; an identical retry returned that ID.")
    reply_text = "Reviewed from the REST desk."
    script = path / "input.json"
    script.write_text(json.dumps([{"t": 0.1, "type": "tap", "key": f"thread-{root['id']}"},
                                  {"t": 0.4, "type": "mark", "name": "thread_open"}]))

    def rune(name, realtime=False):
        return [ui, "--script", script, "--tree", path / f"{name}.json", "--snap", path / f"{name}.png",
                "--size", "1000x690", "--scale", "1", "--fps", "12", "--at", "3" if realtime else "0.8",
                *(["--realtime"] if realtime else [])]

    def visible(name):
        tree = json.loads((path / f"{name}.json").read_text())
        texts = [e.get("text") for e in tree["elements"]]
        assert root["text"] in texts and reply_text in texts and not tree["warnings"], texts
        assert (path / f"{name}.png").read_bytes()[:8] == b"\x89PNG\r\n\x1a\n"

    with server() as request:
        status, _, thread = request("GET", f"/threads/{root['id']}")
        assert status == 200 and thread["message"] == root and not thread["replies"]
        with child(rune("live", True), env) as (proc, until):
            until(r"rune-mark thread_open ")
            data = {"reply_to": root["id"], "after": root["id"], "text": reply_text}
            status, location, reply = request("POST", "/messages", data, "review")
            assert status == 201 and location == f"/v1/messages/{reply['id']}"
            assert reply["actor"] == "ada/desk" and reply["re"] == root["id"]
            status, _, retry = request("POST", "/messages", data, "review")
            assert status == 200 and retry == reply
            assert proc.wait(timeout=45) == 0
        visible("live")
        event = command("wait", str(root["id"]), "--timeout", "0")["event"]
        assert event["message"]["id"] == reply["id"]
        print(f"REST read the thread and created reply #{reply['id']}; the already-open Rune window rendered it.")

    # Neither the API nor the original inspector is running during this read/backup.
    assert command("read", str(root["id"]))["replies"] == [reply]
    command("backup", str(path / "backup.sqlite3"))
    with server() as request:
        status, _, persisted = request("GET", f"/threads/{reply['id']}")
        assert status == 200 and persisted["message"] == root and persisted["replies"] == [reply]
    subprocess.run(rune("restarted"), env=env, check=True, capture_output=True, timeout=45)
    visible("restarted")
    print("Fresh CLI, REST server and Rune processes read the same persisted thread. Online backup written.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, default=Path(__file__).resolve().parents[1] / "target/debug")
    parser.add_argument("--artifacts", type=Path, help="create a NEW directory for the test database, backup, trees and PNGs")
    args = parser.parse_args()
    if args.artifacts is not None:
        args.artifacts.mkdir(mode=0o700, parents=True, exist_ok=False)
        exercise(args.artifacts.resolve(), args.bin_dir.resolve())
        print("Evidence:", args.artifacts.resolve())
    else:
        with tempfile.TemporaryDirectory(prefix="board-roundtrip-") as scratch:
            exercise(Path(scratch), args.bin_dir.resolve())


if __name__ == "__main__":
    main()
