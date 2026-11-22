"""Small stdio MCP client for the disposable handoff fixture."""
import json
import os
from queue import Queue
import subprocess
from threading import Thread


class Client:
    def __init__(self, exe, db, worker=None, *, mode=None):
        env = {k: v for k, v in os.environ.items() if not k.startswith("BOARD_")}
        args = [exe, "--db", db]
        if worker:
            args += ["--worker", f"review/{worker}"]
        args += ["mcp"] + ([mode] if mode else [])
        self.p = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  env={**env, "BOARD_USER": "fixture"}, text=True, encoding="utf-8")
        self.lines = Queue()
        self.reader = Thread(target=self.read_lines, daemon=True)
        self.reader.start()
        self.seq = 0
        self.rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                "clientInfo": {"name": "handoff-fixture", "version": "1"}})
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
