"""Exercise an eight-worker MCP pool against a disposable Board. No model calls."""
import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import ExitStack
import hashlib
import json
import os
from pathlib import Path
from queue import Queue
import subprocess
import tempfile
from threading import Event, Thread
import time


class Client:
    def __init__(self, exe, db, worker=None, *, mode=None):
        env = {k: v for k, v in os.environ.items() if not k.startswith("BOARD_")}
        args = [exe, "--db", db]
        if worker:
            args += ["--worker", f"pool/{worker}"]
        args += ["mcp"] + ([mode] if mode else [])
        self.p = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  env={**env, "BOARD_USER": "fixture"}, text=True, encoding="utf-8")
        self.lines = Queue()
        self.reader = Thread(target=self.read_lines, daemon=True)
        self.reader.start()
        self.seq = 0
        self.rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                "clientInfo": {"name": "pool-fixture", "version": "1"}})
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def read_lines(self):
        for line in self.p.stdout:
            self.lines.put(line)
        self.lines.put(None)

    def send(self, frame):
        self.p.stdin.write(json.dumps(frame) + "\n")
        self.p.stdin.flush()

    def rpc(self, method, params):
        self.seq += 1
        self.send({"jsonrpc": "2.0", "id": self.seq, "method": method, "params": params})
        line = self.lines.get(timeout=15)
        assert line, "MCP disconnected"
        reply = json.loads(line)
        assert reply["id"] == self.seq and "result" in reply, reply
        return reply["result"]

    def call(self, tool, *, allow=(), **args):
        reply = self.rpc("tools/call", {"name": tool, "arguments": args})
        value = reply["structuredContent"]
        if reply["isError"] and value.get("code") not in allow:
            raise RuntimeError(f"{tool}: {value}")
        return value

    def close(self):
        if self.p.poll() is None:
            self.p.kill()
        self.p.wait(timeout=5)
        self.reader.join(5)
        self.p.stdin.close()
        self.p.stdout.close()


def graph():
    def item(key, text, needs=()):
        return {"key": key, "needs": needs,
                "text": text + "\n\nReturn the integer contribution in decimal.", "no_sync": True}
    leaves = [item(f"leaf-{i}", f"Investigate {i}") for i in range(1, 101)]
    groups = [item(f"group-{i}", "Sum ten investigations",
                   [v["key"] for v in leaves[i*10:(i+1)*10]]) for i in range(10)]
    return leaves + groups + [item("synthesis", "Sum the ten groups", [g["key"] for g in groups])]


def exercise(exe, db, stack):
    def connect(worker=None, *, mode=None):
        client = Client(exe, db, worker, mode=mode)
        stack.callback(client.close)
        return client

    boss = connect()
    boss.call("swarm_start", slug="pool", goal="Sum investigations", max_active=8)
    tasks = graph()

    def submit_plan():
        made = boss.call("tasks_plan", channel="swarms/pool", key="investigate", tasks=tasks)
        return {item["key"]: task["id"] for item, task in zip(tasks, made["tasks"], strict=True)}

    ids = submit_plan()
    workers = []
    for i in range(8):
        name = f"worker-{i}"
        boss.call("swarm_add", run="pool", name=name, brief="Claim and sum fixture items")
        worker = connect(name, mode="--queue")
        assert worker.call("worker_assignment")["worker"]["name"] == name
        workers.append(worker)
    kickoff = boss.call("board_post", channel="swarms/pool",
                        text="Queue is ready: " + " ".join(f"@worker-{i}" for i in range(8)))
    for c in workers:
        batch = c.call("swarm_wait", timeout_seconds=0)
        assert [m["id"] for m in batch["messages"]] == [kickoff["id"]]
        assert c.call("swarm_wait", ack=batch["cursor"], timeout_seconds=0)["status"] == "still_waiting"
    assert boss.call("swarm_observe", run="pool")["available_capacity"] == 0
    queue = {"ready_channel": "swarms/pool", "lease_seconds": 30}
    lost, stale = [c.call("tasks_claim", **queue) for c in workers[:2]]
    workers[0].close()  # abrupt loss, including its private proof
    workers[0] = connect("worker-0", mode="--queue")
    assert workers[0].call("worker_assignment")["worker"]["name"] == "worker-0"
    request = workers[1].call("board_post", channel="swarms/pool", text="Confirm synthesis format",
                              schema="decision-request@1",
                              schema_data={"to": "fixture", "summary": "Use decimal output?"})
    first_settlement, abort = Event(), Event()
    deadline = time.monotonic() + 100

    def drain(c):
        finished = {}
        try:
            while not abort.is_set() and time.monotonic() < deadline:
                task = c.call("tasks_claim", **queue, wait_seconds=2, allow=("nothing_ready",))
                if task.get("code") == "nothing_ready":
                    if c.call("swarm_observe", limit=1)["tasks"].get("completed") == 111:
                        return finished
                    continue
                tid = task["id"]
                if task["needs"]:
                    inputs = task["inputs"]
                    assert [item["id"] for item in inputs] == task["needs"]
                    assert all(t["state"] == "completed" for t in inputs)
                    value = sum(int(t["result"]) for t in inputs)
                else:
                    value = int(task["text"].splitlines()[0].split()[1])
                if tid == ids["synthesis"]:
                    digest = hashlib.sha256(str(value).encode()).hexdigest()
                    c.call("board_post", reply_to=tid, text="Verify final sum", schema="handoff@1",
                           schema_data={"task": tid, "revision": {"kind": "sha256", "value": digest},
                                        "to": "worker-1", "summary": "Final sum", "changes": ["sum.txt"]})
                assert c.call("tasks_complete", id=tid, result=str(value))["state"] == "completed"
                assert tid not in finished
                finished[tid] = task["epoch"]
                first_settlement.set()
            raise RuntimeError("pool did not drain within its deadline")
        except BaseException:
            abort.set()
            first_settlement.set()
            raise

    with ThreadPoolExecutor(max_workers=7) as pool:
        jobs = [pool.submit(drain, c) for c in [workers[0], *workers[2:]]]
        try:
            assert first_settlement.wait(10)
            boss.close()  # workers keep working, no new agent is launched by Board
            boss = connect()
            view = boss.call("swarm_observe", run="pool", limit=1)
            assert len(view["changes"]) <= 1 and view["actions"][0]["message_id"] == request["id"]
            assert submit_plan() == ids
            boss.call("board_post", reply_to=request["id"], text="Use decimal", schema="decision-resolution@1",
                      schema_data={"request": request["id"], "summary": "Use decimal"})
            results = [job.result(timeout=100) for job in jobs]
        finally:
            abort.set()

    epochs = {tid: epoch for result in results for tid, epoch in result.items()}
    assert set(epochs) == set(ids.values()) and sum(map(len, results)) == 111
    for old in (lost, stale):
        assert epochs[old["id"]] > old["epoch"]
    verifier = workers[1]
    assert verifier.call("tasks_complete", id=stale["id"], allow=("claim_lost",))["code"] == "claim_lost"
    final = verifier.call("board_read", target=ids["synthesis"])["message"]["task"]
    assert final["result"] == str(sum(range(1, 101))) == "5050"
    handoff = verifier.call("swarm_observe", explain_task=ids["synthesis"])["steps"][0]["handoff"]
    revision = {"kind": "sha256", "value": hashlib.sha256(b"5050").hexdigest()}
    assert handoff["revision"] == revision
    verifier.call("board_post", reply_to=ids["synthesis"], text="Checked final sum", schema="verification@1",
                  schema_data={"handoff": handoff["id"], "revision": revision, "summary": "Passed",
                               "checks": [{"name": "Independent sum", "command": "sum(range(1, 101))",
                                           "status": "pass", "output": "5050"}]})
    receipt = connect(mode="--full").call("swarm_receipt", run="pool")
    assert not receipt["open_tasks"] and not receipt["decisions"] and receipt["handoffs"][0]["status"] == "pass"
    assert boss.call("swarm_observe", run="pool")["actions"] == []
    boss.call("swarm_stop", run="pool")
    print("111 unique settlements; 8 worker identities; 2 expired claims reclaimed; "
          "coordinator restarted; stale settlement rejected; independently verified result: 5050")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, default=Path("target/debug/board"))
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as tmp, ExitStack() as stack:
        exercise(args.bin.resolve(), Path(tmp) / "pool.sqlite3", stack)
