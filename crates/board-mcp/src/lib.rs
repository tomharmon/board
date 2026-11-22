//! Board's local MCP transport and tool catalog. Tool calls always go through
//! the same authenticated Board connection as CLI or HTTP operations.

mod router;
mod stdio;
mod tools;

use router::ActorContext;
pub use stdio::serve_stdio_with_tools;

/// What this connection advertises and accepts, not an authorization grant.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Toolset {
    #[default]
    Board,
    Full,
}
