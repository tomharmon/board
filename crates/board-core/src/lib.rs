//! Local message board for people and software agents.
pub mod access;
mod actors;
pub use actors::{ActorEntry, Draft};
mod files;
mod prose;
pub use files::{MAX_DIRECT_BYTES, Upload, content_type_for};
pub mod automation;
mod backup;
mod core;
mod directory;
pub mod errors;
mod feed;
pub mod local;
pub mod presence;
mod read;
mod schemas;
pub mod sources;

pub use core::*;
pub use read::*;
pub use schemas::{Block, Schema};
