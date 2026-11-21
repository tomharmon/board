use std::io::{self, BufRead, Write};

use anyhow::Result;
use board_local::Board;
use serde_json::Value;

use crate::{ActorContext, handle_request, router::error};

const MAX_FRAME: usize = 2 * 1024 * 1024;

pub fn serve_stdio(board: &Board, actor: &str) -> Result<()> {
    let mut session = ActorContext::new(actor, std::env::var("BOARD_SWARM_RUN").ok());
    serve(board, &mut session, io::stdin().lock(), io::stdout().lock())
}

fn serve(
    board: &Board,
    session: &mut ActorContext,
    mut input: impl BufRead,
    mut output: impl Write,
) -> Result<()> {
    // Discard oversize frames up to their newline without buffering arbitrary input.
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        let mut overflow = false;
        loop {
            let buf = input.fill_buf()?;
            if buf.is_empty() {
                if bytes.is_empty() && !overflow {
                    return Ok(());
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
        let reply = if overflow {
            Some(error(Value::Null, -32700, "JSON frame exceeds 2 MiB"))
        } else {
            match serde_json::from_slice(&bytes) {
                Ok(request) => handle_request(board, session, request),
                Err(_) => Some(error(Value::Null, -32700, "Parse error")),
            }
        };
        if let Some(reply) = reply {
            serde_json::to_writer(&mut output, &reply)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
}
