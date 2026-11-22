use anyhow::Result;
use board_local::{Board, local};
use board_mcp::Toolset;

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let toolset = match args.as_slice() {
        [] => Toolset::Board,
        [arg] if arg == "--queue" => Toolset::Queue,
        [arg] if arg == "--full" => Toolset::Full,
        _ => anyhow::bail!("usage: board-mcp-stdio [--queue | --full]"),
    };
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
    board_mcp::serve_stdio_with_tools(
        &board,
        &actor,
        local::identity_env("BOARD_WORKER")?.as_deref(),
        toolset,
    )
}
