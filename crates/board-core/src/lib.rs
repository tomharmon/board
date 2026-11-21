//! Local message board for people and software agents.
pub mod access;
mod actors;
pub use actors::{ActorEntry, Draft, MentionActor, MentionResolution};
mod drafts;
pub use drafts::{ComposerSettings, SavedComposer, TaskDraftSettings};
mod files;
mod prose;
pub mod reactions;
pub use files::{
    MAX_DIRECT_BYTES, Upload, content_type_for, valid_name as validate_attachment_name,
};
pub mod automation;
mod backup;
mod core;
mod directory;
pub mod errors;
mod feed;
pub mod links;
pub mod local;
pub mod presence;
mod read;
mod schemas;
pub mod sources;

pub use core::*;
pub use read::*;
pub use schemas::{Block, Schema, append_block};
mod projections;
pub mod roles;
pub mod swarm;
