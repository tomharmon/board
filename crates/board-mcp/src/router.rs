use std::collections::HashMap;

use board_local::Board;
use serde_json::{Value, json};

/// A single initialized client and its private, fenced task leases. Never
/// select an actor or accept a credential supplied in a tool's arguments.
pub struct ActorContext {
    pub(crate) actor: String,
    pub(crate) default_run: Option<String>,
    pub(crate) leases: HashMap<i64, (i64, String)>,
    initialized: bool,
}

impl ActorContext {
    pub fn new(actor: &str, default_run: Option<String>) -> Self {
        Self {
            actor: actor.to_owned(),
            default_run,
            leases: HashMap::new(),
            initialized: false,
        }
    }
}

pub(crate) fn error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

/// Dispatch one decoded JSON-RPC message; notifications never get replies.
pub fn handle_request(board: &Board, session: &mut ActorContext, request: Value) -> Option<Value> {
    let method = request.get("method").and_then(Value::as_str);
    let id = request.get("id");
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || method.is_none()
        || id.is_some_and(|v| !v.is_string() && !v.is_i64() && !v.is_u64())
    {
        return Some(error(Value::Null, -32600, "Invalid Request"));
    }
    let id = id?.clone();
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
            json!({"tools":crate::tools::catalog()})
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
            match crate::tools::execute(board, session, tool, args) {
                Ok(v) => json!({"content":[{"type":"text","text":v.to_string()}],"isError":false}),
                Err(e) => {
                    let (_, code) = board_local::errors::classify(&e);
                    let detail = json!({"code":code,"message":e.to_string()});
                    json!({"content":[{"type":"text","text":detail.to_string()}],"isError":true})
                }
            }
        }
        _ => return Some(error(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}
