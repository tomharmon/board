use anyhow::Result;
use board_local::{Board, local};

fn main() -> Result<()> {
    let mut board = Board::open(local::database_path())?;
    let actor = if let Some(actor) = board.bind_env()? {
        actor
    } else if let Some(profile) = local::identity_env("BOARD_PROFILE")? {
        format!(
            "{}/{}",
            local::person_name(),
            local::agent_profile_name(&profile)?
        )
    } else {
        local::person_name()
    };
    board_mcp::serve_stdio(&board, &actor)
}
