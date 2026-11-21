use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

// Capture to anonymous local files so a full stdout/stderr pipe cannot deadlock setup.
pub(super) fn invoke(mut cmd: Command, input: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut out = tempfile::tempfile()?;
    let err = tempfile::tempfile()?;
    cmd.stdout(out.try_clone()?)
        .stderr(err)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut child = cmd.spawn().context("could not launch local executable")?;
    if let Some(bytes) = input
        && let Err(e) = child
            .stdin
            .take()
            .context("missing child stdin")?
            .write_all(bytes)
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e.into());
    }
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(status.success(), "local command exited with {status}");
            break;
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            bail!("local command timed out");
        }
        thread::sleep(Duration::from_millis(20));
    }
    bounded(&mut out)
}
fn bounded(file: &mut File) -> Result<Vec<u8>> {
    ensure!(
        file.metadata()?.len() <= 1024 * 1024,
        "local command produced more than 1 MiB of output"
    );
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = vec![];
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}
pub(super) fn verify(exe: &Path, scratch: &Path) -> Result<()> {
    let mut cmd = Command::new(exe);
    cmd.arg("mcp").current_dir(scratch);
    for (k, _) in std::env::vars_os().filter(|(k, _)| {
        k.to_string_lossy()
            .get(..6)
            .is_some_and(|v| v.eq_ignore_ascii_case("BOARD_"))
    }) {
        cmd.env_remove(k);
    }
    cmd.env("BOARD_DB", scratch.join("probe.sqlite3"))
        .env("BOARD_USER", "init-check");
    let lines = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"board-init","version":env!("CARGO_PKG_VERSION")}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
    ];
    let mut input = Vec::new();
    for item in lines {
        serde_json::to_writer(&mut input, &item)?;
        input.push(b'\n');
    }
    let out = invoke(cmd, Some(&input))?;
    let replies: Vec<Value> = String::from_utf8(out)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    ensure!(
        replies.len() == 2
            && replies[0]["jsonrpc"] == "2.0"
            && replies[0]["id"] == 1
            && replies[0]["result"]["serverInfo"]["name"] == "board-mcp"
            && replies[1]["jsonrpc"] == "2.0"
            && replies[1]["id"] == 2
            && replies[1]["result"]["tools"]
                .as_array()
                .is_some_and(|items| items
                    .iter()
                    .any(|t| t["name"] == "swarm_start" && t["inputSchema"]["type"] == "object")),
        "expected a Board stdio server exposing swarm_start; did not register it"
    );
    Ok(())
}
