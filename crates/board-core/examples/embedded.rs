//! A local client uses the same transactions as the CLI, REST handler and inspector.
use anyhow::{Context, Result};
use board_local::{Board, Filter, PostOptions, local};

fn main() -> Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .context("supply a path to a local board")?;
    let mut board = Board::open(path)?;
    let actor = board.bind_env()?.unwrap_or_else(local::actor_name);
    let first = board.post(
        "examples/rust",
        &actor,
        "Opened from Rust",
        &PostOptions {
            key: Some("embedding-example".into()),
            ..Default::default()
        },
    )?;
    let snapshot = board.conversation(first.id, &Filter::default())?;
    println!(
        "Thread #{} has {} replies at cursor {}.",
        first.id,
        snapshot.replies.len(),
        snapshot.cursor
    );
    println!(
        "Follow it with: board tail --re {} --after {}",
        first.id, snapshot.cursor
    );
    Ok(())
}
