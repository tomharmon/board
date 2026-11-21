use anyhow::Result;
use board_local::{Board, local};

fn main() -> Result<()> {
    let mut board = Board::open(local::database_path())?;
    let actor = board.bind_env()?.unwrap_or_else(local::actor_name);
    board_mcp::serve_stdio(&board, &actor)
}
