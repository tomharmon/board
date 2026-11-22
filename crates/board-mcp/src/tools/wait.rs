use super::{ActorContext, catalog, selected_run};
use anyhow::Result;
use board_local::{
    Board,
    errors::{self, Kind},
    swarm::SwarmInboxWait,
};
use serde_json::Value;
use std::time::{Duration, Instant};

pub(crate) struct Parked<'a> {
    pub id: Value,
    deadline: Instant,
    pub next_poll: Instant,
    inbox: SwarmInboxWait<'a>,
}

impl<'a> Parked<'a> {
    pub fn prepare(
        board: &'a Board,
        ctx: &ActorContext,
        args: catalog::SwarmWait,
        id: Value,
    ) -> Result<Self> {
        let run = selected_run(ctx, args.run)?;
        let now = Instant::now();
        Ok(Self {
            id,
            inbox: board.swarm_inbox_wait(
                &ctx.actor,
                &run,
                args.after.map(|x| x.0),
                args.ack.map(|x| x.0),
            )?,
            deadline: now + Duration::from_secs(args.timeout_seconds.map_or(90, |x| x.0)),
            next_poll: now,
        })
    }

    pub fn poll(&mut self) -> Result<Option<Value>> {
        let now = Instant::now();
        let expired = now >= self.deadline;
        self.next_poll = (now + Duration::from_millis(200)).min(self.deadline);
        match self.inbox.try_poll(expired) {
            Err(e) if errors::classify(&e).0 == Kind::Busy && !expired => Ok(None),
            result => result?.map(super::value).transpose(),
        }
    }
}
