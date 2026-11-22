use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

pub(super) struct Client {
    pub(super) child: Child,
    input: ChildStdin,
    lines: mpsc::Receiver<String>,
    id: u64,
}
impl Client {
    pub(super) fn spawn(mut cmd: Command, mode: Option<&str>) -> Self {
        cmd.arg("mcp").args(mode);
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            lines,
            id: 0,
        }
    }
    pub(super) fn write(&mut self, s: &str) {
        writeln!(self.input, "{s}").unwrap();
        self.input.flush().unwrap();
    }
    pub(super) fn read(&self) -> Value {
        let line = self.lines.recv_timeout(Duration::from_secs(10)).expect(
            "expected one complete protocol frame; no diagnostic output is permitted on stdout",
        );
        serde_json::from_str(&line).unwrap()
    }
    pub(super) fn send(&mut self, method: &str, params: Value) -> u64 {
        self.id += 1;
        self.write(
            &json!({"jsonrpc":"2.0","method":method,"id":self.id,"params":params}).to_string(),
        );
        self.id
    }
    pub(super) fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params);
        let msg = self.read();
        assert_eq!(msg["id"], id, "{msg}");
        msg
    }
    pub(super) fn initialize(&mut self) {
        let r=self.request("initialize",json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"pipe-test","version":"1"}}));
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(r["result"]["serverInfo"]["name"], "board-mcp");
        assert_eq!(r["result"]["capabilities"], json!({"tools":{}}));
        self.write(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        assert_eq!(self.request("ping", json!({}))["result"], json!({}));
    }
    pub(super) fn tool(&mut self, name: &str, args: Value, success: bool) -> Value {
        let reply = self.request("tools/call", json!({"name":name,"arguments":args}));
        Self::content(reply, success)
    }
    pub(super) fn content(reply: Value, success: bool) -> Value {
        assert_eq!(reply["result"]["isError"], !success, "{reply}");
        assert_eq!(reply["result"]["content"][0]["type"], "text");
        serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    }
    pub(super) fn call(&mut self, name: &str, args: Value) -> Value {
        self.tool(name, args, true)
    }
    pub(super) fn denied(&mut self, name: &str, args: Value) -> Value {
        self.tool(name, args, false)
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
