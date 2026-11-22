use std::{cell::OnceCell, collections::HashMap};

use crate::{Toolset, tools::wait::Parked};
use board_local::Board;
use serde_json::{Value, json};

/// A single initialized client and its private, fenced task leases. Never
/// select an actor or accept a credential supplied in a tool's arguments.
pub struct ActorContext {
    pub(crate) actor: String,
    pub(crate) default_run: Option<String>,
    pub(crate) worker: Option<board_local::swarm::WorkerBinding>,
    pub(crate) leases: HashMap<i64, (i64, String)>,
    pub(crate) toolset: Toolset,
    initialized: bool,
    catalog: OnceCell<Vec<crate::tools::Published>>,
}

impl ActorContext {
    pub fn new(actor: &str, default_run: Option<String>) -> Self {
        Self {
            actor: actor.to_owned(),
            default_run,
            worker: None,
            leases: HashMap::new(),
            toolset: Toolset::Board,
            initialized: false,
            catalog: OnceCell::new(),
        }
    }

    pub fn with_toolset(mut self, toolset: Toolset) -> Self {
        self.toolset = toolset;
        self.catalog.take();
        self
    }

    fn catalog(&self, board: &Board) -> &[crate::tools::Published] {
        self.catalog
            .get_or_init(|| crate::tools::catalog(board, self.toolset, self.worker.is_some()))
    }

    fn instructions(&self) -> String {
        let mut hint = if self.worker.is_some() {
            "Start with worker_assignment for your brief. "
        } else {
            "Use swarm_start, swarm_add and swarm_stop to coordinate named workers; Board never starts or stops their processes. Use tasks_plan for ordinary tasks and dependencies. "
        }.to_owned();
        hint.push_str("Use board_post to post or reply to a task, board_read for the feed or a thread, swarm_wait for addressed messages, and swarm_observe for progress and blockers. Reading does not acknowledge a message.");
        if self.toolset != Toolset::Board {
            hint.push_str(
                " Claim a task by ID and renew, release or settle it on this connection.",
            );
        }
        hint
    }
}

pub(crate) fn error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

pub(crate) fn poll(wait: &mut Parked<'_>) -> Option<Value> {
    let result = wait.poll().transpose()?;
    Some(json!({"jsonrpc":"2.0","id":wait.id,"result":tool_result(result)}))
}

fn tool_result(result: anyhow::Result<Value>) -> rmcp::model::CallToolResult {
    use rmcp::model::CallToolResult;
    match result {
        Ok(v) => {
            let is_object = v.is_object();
            let mut result = CallToolResult::structured(v);
            if !is_object {
                result.structured_content =
                    Some(json!({"items": result.structured_content.take()}));
            }
            result
        }
        Err(e) => {
            let (_, code) = board_local::errors::classify(&e);
            CallToolResult::structured_error(json!(crate::tools::ToolOutput::<()>::Error {
                code: code.into(),
                message: e.to_string(),
            }))
        }
    }
}

pub(crate) fn route<'a>(
    board: &'a Board,
    session: &mut ActorContext,
    request: Value,
    parked: &mut Option<Parked<'a>>,
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
        && parked.as_ref().is_some_and(|p| &p.id == cancelled)
    {
        *parked = None;
    }
    let id = id?.clone();
    if parked.as_ref().is_some_and(|p| p.id == id) {
        return Some(error(id, -32600, "Request ID already in use"));
    }
    let method = method?;
    let params = request.get("params");
    if params.is_some_and(|p| !p.is_object()) {
        return Some(error(id, -32602, "Expected an object for params"));
    }
    let result = match method {
        "initialize" if !session.initialized => {
            let supported = ["2025-11-25", "2025-06-18"];
            let requested = params
                .and_then(|v| v.get("protocolVersion"))
                .and_then(Value::as_str);
            let version = requested
                .filter(|v| supported.contains(v))
                .unwrap_or(supported[0]);
            session.initialized = true;
            json!({"protocolVersion":version,"capabilities":{"tools":{}},
                "instructions":session.instructions(),
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
            json!({"tools":session.catalog(board).iter().map(|t| &t.definition).collect::<Vec<_>>()})
        }
        "tools/call" => {
            let name = params.and_then(|p| p.get("name")).and_then(Value::as_str);
            let Some(published) = session
                .catalog(board)
                .iter()
                .find(|t| Some(t.tool.name) == name)
            else {
                return Some(error(
                    id,
                    -32602,
                    "Tool not available in this catalog; use tools/list",
                ));
            };
            let empty = json!({});
            let args = params.and_then(|p| p.get("arguments")).unwrap_or(&empty);
            let result = match published.parse(args) {
                Err(e) => Err(e),
                Ok(crate::tools::Call::SwarmWait(_)) if parked.is_some() => Err(anyhow::anyhow!(
                    "this session already has an inbox wait pending"
                )),
                Ok(crate::tools::Call::SwarmWait(p)) => {
                    match Parked::prepare(board, session, p, id.clone()) {
                        Ok(mut wait) => match wait.poll() {
                            Ok(None) => {
                                *parked = Some(wait);
                                return None;
                            }
                            Ok(Some(v)) => Ok(v),
                            Err(e) => Err(e),
                        },
                        Err(e) => Err(e),
                    }
                }
                Ok(call) => crate::tools::execute(board, session, call),
            };
            json!(tool_result(result))
        }
        _ => return Some(error(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}
