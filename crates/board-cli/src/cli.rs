use clap::{Args as Group, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "board",
    version,
    about = "One local message board for people and agents"
)]
pub struct Args {
    /// SQLite file (default: the board in your local data directory)
    #[arg(long, global = true, env = "BOARD_DB")]
    pub db: Option<PathBuf>,
    /// Named board in your local data directory (case-insensitive; never a remote board)
    #[arg(long, global = true, conflicts_with = "db")]
    pub name: Option<String>,
    /// Write as a named agent of the local user
    #[arg(long = "as", global = true, env = "BOARD_PROFILE")]
    pub profile: Option<String>,
    /// Emit machine-readable JSON; tail emits JSON Lines
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub cmd: Command,
}

#[derive(Group)]
pub struct Body {
    /// Message text; use - for stdin
    pub text: Option<String>,
    /// Take message text from a file instead
    #[arg(long, conflicts_with = "text")]
    pub file: Option<PathBuf>,
    /// Interpret text as JSON for a registered name@version
    #[arg(long)]
    pub schema: Option<String>,
}

#[derive(Group)]
pub struct WriteOptions {
    /// Keep this message local, even in a mirrored channel
    #[arg(long)]
    pub no_sync: bool,
    #[command(flatten)]
    pub body: Body,
    #[arg(long, value_name = "KEY=VALUE")]
    pub label: Vec<String>,
    #[arg(long, value_name = "FILE")]
    pub attach: Vec<PathBuf>,
    /// Carry an existing upload by its ID or opaque key
    #[arg(long, value_name = "ID_OR_KEY")]
    pub attachment_id: Vec<String>,
    /// Retry this write without duplicating it
    #[arg(long = "idempotency-key", visible_alias = "id")]
    pub key: Option<String>,
    /// Append only if this is still the newest message in the thread
    #[arg(long,value_parser=crate::parse_id)]
    pub after: Option<i64>,
}

#[derive(Group)]
pub struct Query {
    #[arg(long, visible_alias = "source")]
    pub from: Option<String>,
    #[arg(long)]
    pub by: Option<String>,
    #[arg(long)]
    pub text: Option<String>,
    #[arg(long,value_parser=crate::parse_id)]
    pub re: Option<i64>,
    #[arg(long, num_args=0..=1, default_missing_value="")]
    pub mentions: Option<String>,
    #[arg(long, value_name = "KEY[=VALUE]")]
    pub label: Vec<String>,
    #[arg(long)]
    pub after: Option<i64>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Set channel permissions for exact local actors, their profiles, or everyone
    Grants {
        #[command(subcommand)]
        command: crate::access_cli::Grants,
    },
    /// Issue and revoke actor-bound bearer credentials for this board
    Credentials {
        #[command(subcommand)]
        command: crate::access_cli::Credentials,
    },
    /// Run the optional bearer-authenticated REST server on 127.0.0.1
    Serve {
        #[arg(long, default_value_t = 4100)]
        port: u16,
    },
    /// Make a consistent SQLite backup to a new file, even while writers are active
    Backup { destination: PathBuf },
    /// Show the local author handle and database
    Whoami,
    /// Describe this release's local API and capabilities without a running server
    Discovery,
    /// Exercise the installed local engine on a disposable database, never the selected board.
    Conformance,
    /// Inspect the machine-readable CLI command tree without opening the database
    Commands,
    /// Manage bounded, local post/edit middleware (owner only)
    Rules {
        #[command(subcommand)]
        command: Option<RuleCommand>,
    },
    /// Set a short-lived typing status in a local thread without recording a board event
    Typing {
        #[arg(value_parser=crate::parse_id)]
        id: i64,
        status: Option<String>,
        #[arg(long, conflicts_with = "status")]
        clear: bool,
        #[arg(long,conflicts_with_all=["status","clear"])]
        list: bool,
    },
    /// Configure optional sources. Adding one does not contact a provider.
    Sources {
        #[command(subcommand)]
        command: Option<crate::sync_cli::Sources>,
    },
    /// Import, link or explicitly synchronize external conversations.
    Sync {
        #[command(subcommand)]
        command: crate::sync_cli::Sync,
    },
    /// Inspect and consume the durable per-thread connector queues.
    Inbox {
        #[command(subcommand)]
        command: crate::sync_cli::Inbox,
    },
    /// List channels
    Channels {
        #[arg(long, conflicts_with = "scope")]
        prefix: Option<String>,
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// List person and agent handles that have written to the board
    Actors {
        #[arg(long)]
        details: bool,
        #[arg(long)]
        query: Option<String>,
        /// Exact actor IDs; repeat to request multiple cards.
        #[arg(long = "id")]
        ids: Vec<String>,
    },
    /// Set the current actor's display name and mention handle
    Profile {
        #[arg(long)]
        handle: String,
        #[arg(long)]
        name: String,
        /// HTTPS metadata only; use an empty string to remove an existing avatar.
        #[arg(long)]
        avatar: Option<String>,
    },
    /// Create an empty channel (posting also creates one)
    Channel { name: String },
    /// Start a thread in a channel, or reply to a message ID
    Post {
        target: String,
        #[command(flatten)]
        options: WriteOptions,
    },
    /// Look up the message recorded under one of your client-supplied keys
    Posted { key: String },
    /// Reply to any message in a thread
    Reply {
        #[arg(value_parser=crate::parse_id)]
        id: i64,
        #[command(flatten)]
        options: WriteOptions,
    },
    /// Read recent threads, or an entire conversation by ID
    Read {
        target: Option<String>,
        #[command(flatten)]
        query: Query,
        #[arg(long, conflicts_with = "after")]
        before: Option<i64>,
        /// For a thread, return the newest N matching replies; lists default to 50
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        flat: bool,
    },
    /// Replace the text of a message you authored
    Edit {
        #[arg(value_parser=crate::parse_id)]
        id: i64,
        #[command(flatten)]
        body: Body,
        #[arg(long)]
        if_revision: Option<i64>,
    },
    /// Replace a message you authored with a tombstone
    #[command(visible_alias = "rm")]
    Delete {
        #[arg(value_parser=crate::parse_id)]
        id: i64,
        #[arg(long)]
        if_revision: Option<i64>,
    },
    /// Add or remove your reaction
    React {
        #[arg(value_parser=crate::parse_id)]
        id: i64,
        #[arg(allow_negative_numbers = true)]
        emoji: String,
        #[arg(long)]
        remove: bool,
    },
    /// Atomically set and remove labels, optionally checking previous values
    Label {
        #[arg(value_parser=crate::parse_id)]
        id: i64,
        #[arg(value_name = "KEY=VALUE")]
        labels: Vec<String>,
        #[arg(long)]
        unset: Vec<String>,
        #[arg(long = "if")]
        conditions: Vec<String>,
        #[arg(long)]
        if_unset: Vec<String>,
    },
    /// Extract an attachment by file ID (use - for stdout)
    Download { id: String, path: Option<PathBuf> },
    /// Store immutable attachment bytes for one or more posts
    Upload {
        channel: String,
        file: PathBuf,
        #[arg(long = "type", default_value = "application/octet-stream")]
        content_type: String,
    },
    /// Follow the local event log; --after resumes a saved cursor
    Tail {
        target: Option<String>,
        #[command(flatten)]
        query: Query,
        #[arg(long, conflicts_with = "after")]
        last: Option<usize>,
    },
    /// Wait for one matching event, including replies already posted after an ID
    Wait {
        target: Option<String>,
        #[command(flatten)]
        query: Query,
        #[arg(long, default_value = "30m")]
        timeout: String,
    },
    /// Register and inspect versioned JSON schemas for structured messages
    Schemas {
        #[command(subcommand)]
        cmd: Option<SchemaCommand>,
    },
    /// Open the native Rune inspector
    #[command(visible_alias = "ui")]
    Inspect {
        #[arg(long,value_parser=crate::parse_id)]
        thread: Option<i64>,
    },
    #[command(flatten)]
    Automation(crate::automation_cli::Commands),
}
#[derive(clap::Subcommand)]
pub enum RuleCommand {
    List,
    /// Install or update an explicitly named rule; FILE is Rhai source or '-' for stdin
    Set {
        name: String,
        file: PathBuf,
        #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
        priority: i64,
    },
    Remove {
        name: String,
    },
}

#[derive(Subcommand)]
pub enum SchemaCommand {
    List,
    Inspect {
        schema: String,
    },
    Create {
        schema: String,
        #[arg(long, conflicts_with = "data", required_unless_present = "data")]
        file: Option<PathBuf>,
        #[arg(long)]
        data: Option<String>,
        #[arg(long, default_value = "")]
        description: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serve_allows_choosing_a_port_but_not_a_public_bind_address() {
        let args = Args::try_parse_from(["board", "serve", "--port", "0"]).unwrap();
        assert!(matches!(args.cmd, Command::Serve { port: 0 }));
        for opt in ["--host", "--bind", "--listen"] {
            assert!(Args::try_parse_from(["board", "serve", opt, "0.0.0.0"]).is_err());
        }
    }
}
