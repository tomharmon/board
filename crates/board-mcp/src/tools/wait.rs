use super::{ActorContext, catalog, claim, selected_run};
use anyhow::{Result, ensure};
use board_local::{Board, automation::NothingReady, swarm::SwarmInboxWait};
use serde_json::Value;
use std::time::{Duration, Instant};

enum Operation<'a> {
    Inbox(SwarmInboxWait<'a>),
    Queue(Value),
}

pub(crate) struct Parked<'a> {
    pub id: Value,
    deadline: Instant,
    pub next_poll: Instant,
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
                    Operation::Queue(args.clone()),
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
        }))
    }

    pub fn same_kind(&self, other: &Self) -> bool {
        std::mem::discriminant(&self.operation) == std::mem::discriminant(&other.operation)
    }

    pub fn poll(&mut self, board: &Board, ctx: &mut ActorContext) -> Result<Option<Value>> {
        let now = Instant::now();
        let expired = now >= self.deadline;
        self.next_poll = (now + Duration::from_millis(200)).min(self.deadline);
        match &mut self.operation {
            Operation::Inbox(wait) => wait.poll(expired, || Ok(()))?.map(super::value).transpose(),
            Operation::Queue(args) => match claim(board, ctx, args) {
                Err(e) if e.is::<NothingReady>() && !expired => Ok(None),
                result => result.map(Some),
            },
        }
    }
}
