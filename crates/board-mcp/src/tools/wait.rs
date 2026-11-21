use std::time::Duration;

use anyhow::Result;
use board_local::Board;
use serde_json::Value;

pub(super) fn run(
    b: &Board,
    actor: &str,
    run: &str,
    after: Option<i64>,
    seconds: u64,
) -> Result<Value> {
    // Do not intercept termination of the stdio host. A later waiter owns a fresh PID.
    Ok(serde_json::to_value(b.swarm_wait(
        actor,
        run,
        after,
        Duration::from_secs(seconds),
        || Ok(()),
    )?)?)
}
