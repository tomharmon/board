use std::collections::HashMap;

use crate::tools::wait::Parked;
use board_local::Board;
use serde_json::{Value, json};

/// A single initialized client and its private, fenced task leases. Never
/// select an actor or accept a credential supplied in a tool's arguments.
pub struct ActorContext {
    pub(crate) actor: String,
    pub(crate) default_run: Option<String>,
    pub(crate) worker: Option<board_local::swarm::WorkerBinding>,
    pub(crate) leases: HashMap<i64, (i64, String)>,
    initialized: bool,
    structured: bool,
}

impl ActorContext {
    pub fn new(actor: &str, default_run: Option<String>) -> Self {
        Self {
            actor: actor.to_owned(),
            default_run,
            worker: None,
            leases: HashMap::new(),
            initialized: false,
            structured: false,
        }
    }
}

pub(crate) fn error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

/// Dispatch one decoded JSON-RPC message; notifications never get replies.
pub fn handle_request(board: &Board, session: &mut ActorContext, request: Value) -> Option<Value> {
    let mut parked = Vec::new();
    let immediate = route(board, session, request, &mut parked);
    let Some(mut wait) = parked.pop() else {
        return immediate;
    };
    loop {
        std::thread::sleep(
            wait.next_poll
                .saturating_duration_since(std::time::Instant::now()),
        );
        if let Some(reply) = poll(board, session, &mut wait) {
            return Some(reply);
        }
    }
}

pub(crate) fn poll(
    board: &Board,
    session: &mut ActorContext,
    wait: &mut Parked<'_>,
) -> Option<Value> {
    let result = wait.poll(board, session).transpose()?;
    Some(json!({"jsonrpc":"2.0","id":wait.id,"result":tool_result(session,result)}))
}

fn tool_result(ctx: &ActorContext, result: anyhow::Result<Value>) -> Value {
    let (v, error) = match result {
        Ok(v) => (v, false),
        Err(e) => {
            let (_, code) = board_local::errors::classify(&e);
            (json!({"code":code,"message":e.to_string()}), true)
        }
    };
    let mut out = json!({"content":[{"type":"text","text":v.to_string()}],"isError":error});
    if ctx.structured {
        out["structuredContent"] = if v.is_object() { v } else { json!({"items":v}) };
    }
    out
}

pub(crate) fn route<'a>(
    board: &'a Board,
    session: &mut ActorContext,
    request: Value,
    parked: &mut Vec<Parked<'a>>,
) -> Option<Value> {
    let method = request.get("method").and_then(Value::as_str);
    let id = request.get("id");
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || method.is_none()
        || id.is_some_and(|v| !v.is_string() && !v.is_i64() && !v.is_u64())
    {
        return Some(error(Value::Null, -32600, "Invalid Request"));
    }
    if id.is_none()
        && method == Some("notifications/cancelled")
        && let Some(cancelled) = request.pointer("/params/requestId")
    {
        parked.retain_mut(|p| &p.id != cancelled || !p.retire());
    }
    let id = id?.clone();
    if parked.iter().any(|p| p.id == id) {
        return Some(error(id, -32600, "Request ID already in use"));
    }
    let method = method?;
    let params = request.get("params");
    if params.is_some_and(|p| !p.is_object()) {
        return Some(error(id, -32602, "Expected an object for params"));
    }
    let result = match method {
        "initialize" if !session.initialized => {
            let supported = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
            let requested = params
                .and_then(|v| v.get("protocolVersion"))
                .and_then(Value::as_str);
            let version = requested
                .filter(|v| supported.contains(v))
                .unwrap_or(supported[0]);
            session.initialized = true;
            session.structured = version >= "2025-06-18";
            json!({"protocolVersion":version,"capabilities":{"tools":{}},
                "serverInfo":{"name":"board-mcp","version":env!("CARGO_PKG_VERSION")}})
        }
        "initialize" => return Some(error(id, -32600, "Session already initialized")),
        "ping" => json!({}),
        _ if !session.initialized => {
            return Some(error(id, -32002, "Initialize the MCP session first"));
        }
        "tools/list" => {
            if params.and_then(|p| p.get("cursor")).is_some() {
                return Some(error(id, -32602, "This catalog fits on one page"));
            }
            json!({"tools":crate::tools::catalog(board,session.worker.is_some(),session.structured)})
        }
        "tools/call" => {
            let Some(tool) = params
                .and_then(|p| p.get("name"))
                .and_then(Value::as_str)
                .and_then(crate::tools::find)
            else {
                return Some(error(id, -32602, "Unknown or missing Board tool name"));
            };
            let empty = json!({});
            let args = params.and_then(|p| p.get("arguments")).unwrap_or(&empty);
            let result = match Parked::prepare(board, session, tool, args, id.clone()) {
                Ok(Some(wait)) if parked.iter().any(|p| p.same_kind(&wait)) => Err(
                    anyhow::anyhow!("this session already has that kind of wait pending"),
                ),
                Ok(Some(mut wait)) => match wait.poll(board, session) {
                    Ok(None) => {
                        parked.push(wait);
                        return None;
                    }
                    Ok(Some(value)) => Ok(value),
                    Err(e) => Err(e),
                },
                Ok(None) => crate::tools::execute(board, session, tool, args),
                Err(e) => Err(e),
            };
            tool_result(session, result)
        }
        _ => return Some(error(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}
