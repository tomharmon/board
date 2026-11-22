use super::{ActorContext, catalog, remember, selected_run};
use anyhow::{Result, ensure};
use board_local::{
    Board,
    automation::{NothingReady, try_claim_ready_until},
    errors::{self, Kind},
    swarm::SwarmInboxWait,
};
use serde_json::Value;
use std::time::{Duration, Instant};

enum Operation<'a> {
    Inbox(SwarmInboxWait<'a>),
    Queue {
        channel: String,
        labels: std::collections::BTreeMap<String, String>,
        lease_seconds: i64,
    },
}

pub(crate) struct Parked<'a> {
    pub id: Value,
    deadline: Instant,
    pub next_poll: Instant,
    pub cancelled: bool,
    operation: Operation<'a>,
}

impl<'a> Parked<'a> {
    pub fn prepare(
        board: &'a Board,
        ctx: &ActorContext,
        tool: &catalog::Tool,
        args: &Value,
        id: Value,
    ) -> Result<Option<Self>> {
        let (operation, seconds) = match tool.name {
            "swarm_wait" => {
                tool.validate(args)?;
                let run = selected_run(ctx, args)?;
                (
                    Operation::Inbox(board.swarm_inbox_wait(
                        &ctx.actor,
                        &run,
                        args["after"].as_i64(),
                        args["ack"].as_i64(),
                    )?),
                    args["timeout_seconds"].as_u64().unwrap_or(90),
                )
            }
            "tasks_claim" if args["wait_seconds"].as_u64().unwrap_or(0) > 0 => {
                tool.validate(args)?;
                ensure!(
                    args.get("id").is_none() && args["ready_channel"].is_string(),
                    "only a ready queue can wait"
                );
                (
                    Operation::Queue {
                        channel: args["ready_channel"].as_str().unwrap().into(),
                        labels: args
                            .get("labels")
                            .cloned()
                            .map(serde_json::from_value)
                            .transpose()?
                            .unwrap_or_default(),
                        lease_seconds: args["lease_seconds"].as_i64().unwrap_or(300),
                    },
                    args["wait_seconds"].as_u64().unwrap(),
                )
            }
            _ => return Ok(None),
        };
        let now = Instant::now();
        Ok(Some(Self {
            id,
            operation,
            deadline: now + Duration::from_secs(seconds),
            next_poll: now,
            cancelled: false,
        }))
    }

    pub fn same_kind(&self, other: &Self) -> bool {
        std::mem::discriminant(&self.operation) == std::mem::discriminant(&other.operation)
    }

    pub fn poll(&mut self, board: &Board, ctx: &mut ActorContext) -> Result<Option<Value>> {
        let now = Instant::now();
        let expired = now >= self.deadline;
        self.next_poll = (now + Duration::from_millis(200)).min(self.deadline);
        let result = match &mut self.operation {
            Operation::Inbox(wait) => wait
                .try_poll(expired)
                .and_then(|v| v.map(super::value).transpose()),
            Operation::Queue {
                channel,
                labels,
                lease_seconds,
            } => try_claim_ready_until(
                board,
                &ctx.actor,
                channel,
                labels,
                *lease_seconds,
                ctx.worker.as_ref(),
                self.deadline,
            )
            .and_then(|t| remember(ctx, t))
            .map(Some),
        };
        match result {
            Err(e)
                if (e.is::<NothingReady>() || errors::classify(&e).0 == Kind::Busy) && !expired =>
            {
                Ok(None)
            }
            other => other,
        }
    }

    /// Cancellation never acknowledges an inbox. If a writer is contending,
    /// retire the registration on a later tick without blocking stdin.
    pub fn retire(&mut self) -> bool {
        self.cancelled = true;
        self.next_poll = Instant::now() + Duration::from_millis(200);
        match &mut self.operation {
            Operation::Inbox(wait) => !wait
                .try_cancel()
                .is_err_and(|e| errors::classify(&e).0 == Kind::Busy),
            Operation::Queue { .. } => true,
        }
    }
}
