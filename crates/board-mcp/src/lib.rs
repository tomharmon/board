//! Board's local MCP transport and tool catalog. Tool calls always go through
//! the same authenticated Board connection as CLI or HTTP operations.

mod router;
mod stdio;
mod tools;

pub use router::{ActorContext, handle_request};
pub use stdio::{serve_stdio, serve_stdio_bound};
