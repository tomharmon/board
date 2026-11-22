use std::io::{self, BufRead, Write};
use std::{sync::mpsc, time::Instant};

use anyhow::Result;
use board_local::Board;
use serde_json::Value;

use crate::{
    ActorContext, Toolset,
    router::{self, error},
    tools::wait::Parked,
};

const MAX_FRAME: usize = 2 * 1024 * 1024;

pub fn serve_stdio(board: &Board, actor: &str) -> Result<()> {
    serve_stdio_bound(
        board,
        actor,
        board_local::local::identity_env("BOARD_WORKER")?.as_deref(),
    )
}

pub fn serve_stdio_bound(board: &Board, actor: &str, selector: Option<&str>) -> Result<()> {
    serve_stdio_with_tools(board, actor, selector, Toolset::Board)
}

pub fn serve_stdio_with_tools(
    board: &Board,
    actor: &str,
    selector: Option<&str>,
    toolset: Toolset,
) -> Result<()> {
    let mut session =
        ActorContext::new(actor, std::env::var("BOARD_SWARM_RUN").ok()).with_toolset(toolset);
    if let Some(selector) = selector {
        let binding = board.worker_binding(selector)?;
        session.actor.clone_from(&binding.actor);
        session.default_run = Some(binding.run.clone());
        session.worker = Some(binding);
    }
    // Only this reader owns stdin. The event loop owns the Board connection,
    // stdout, and every private claim proof, including while a tool is parked.
    let (send, recv) = mpsc::sync_channel(8);
    std::thread::spawn(move || {
        let mut input = io::stdin().lock();
        loop {
            let frame = read_frame(&mut input);
            let finished = frame.as_ref().map_or(true, Option::is_none);
            if send.send(frame).is_err() || finished {
                break;
            }
        }
    });
    let mut out = io::stdout().lock();
    let mut pending: Vec<Parked<'_>> = vec![];
    loop {
        let next = pending.iter().map(|p| p.next_poll).min();
        let received = match next {
            Some(at) => match recv.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(v) => Some(v),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            },
            None => Some(recv.recv()?),
        };
        if let Some(frame) = received {
            let Some(frame) = frame? else { break };
            let reply = match frame {
                Ok(request) => router::route(board, &mut session, request, &mut pending),
                Err(msg) => Some(error(Value::Null, -32700, msg)),
            };
            if let Some(reply) = reply {
                send_reply(&mut out, &reply)?;
            }
        }
        let mut i = 0;
        while i < pending.len() {
            let wait = &mut pending[i];
            if wait.next_poll <= Instant::now() {
                if !wait.cancelled
                    && let Some(reply) = router::poll(board, &mut session, wait)
                {
                    send_reply(&mut out, &reply)?;
                    wait.cancelled = true;
                }
                if wait.cancelled && wait.retire() {
                    pending.swap_remove(i);
                    continue;
                }
            }
            i += 1;
        }
    }
    Ok(())
}

fn send_reply(out: &mut impl Write, reply: &Value) -> Result<()> {
    serde_json::to_writer(&mut *out, reply)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

fn read_frame(input: &mut impl BufRead) -> io::Result<Option<Result<Value, &'static str>>> {
    // Discard oversize frames up to their newline without buffering arbitrary input.
    let mut bytes = Vec::new();
    let mut overflow = false;
    loop {
        let buf = input.fill_buf()?;
        if buf.is_empty() {
            if bytes.is_empty() && !overflow {
                return Ok(None);
            }
            break;
        }
        let end = buf.iter().position(|&b| b == b'\n');
        let n = end.unwrap_or(buf.len());
        overflow |= bytes.len().saturating_add(n) > MAX_FRAME;
        if !overflow {
            bytes.extend_from_slice(&buf[..n]);
        }
        input.consume(n + usize::from(end.is_some()));
        if end.is_some() {
            break;
        }
    }
    Ok(Some(if overflow {
        Err("JSON frame exceeds 2 MiB")
    } else {
        serde_json::from_slice(&bytes).map_err(|_| "Parse error")
    }))
}
