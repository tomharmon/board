use super::*;
use std::time::{Duration, Instant};

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SwarmWaitOutcome {
    Messages {
        cursor: i64,
        notifications: Vec<SwarmNotification>,
        messages: Vec<Message>,
        dms: Vec<SwarmDm>,
    },
    StillWaiting {
        cursor: i64,
    },
    Stopped {
        cursor: i64,
    },
}

/// Delivery does not consume messages. Only an explicit acknowledgment
/// supplied by the caller advances the participant's durable inbox position.
pub struct SwarmInboxWait<'a> {
    board: &'a Board,
    actor: String,
    run: String,
    after: i64,
    ack: Option<i64>,
    target: String,
    first_contact: bool,
}

fn wait_for<'r>(r: &'r SwarmRun, actor: &str, name: &str) -> Result<Option<&'r SwarmWorker>> {
    if name == "coordinator" && r.operator(actor) {
        return Ok(None);
    }
    r.workers
        .iter()
        .find(|w| w.name == name && w.actor == actor)
        .map(Some)
        .context(Forbidden.error(
            "wrong_swarm_actor",
            "wait for your own worker profile, not another participant",
        ))
}
fn status_event(
    tx: &Transaction,
    r: &SwarmRun,
    actor: &str,
    name: &str,
    status: &str,
) -> Result<()> {
    Board::event_tx(
        tx,
        "swarm.status",
        actor,
        Some(r.thread_id),
        serde_json::json!({"run":r.slug,"worker":name,"status":status}),
    )?;
    Ok(())
}
impl Board {
    /// Park the current process on its own inbox. The caller supplies any cancellation policy;
    /// no signal handler or agent runtime is installed by the domain layer.
    pub fn swarm_wait(
        &self,
        actor: &str,
        run: &str,
        after: Option<i64>,
        ack: Option<i64>,
        timeout: Duration,
        mut check_cancel: impl FnMut() -> Result<()>,
    ) -> Result<SwarmWaitOutcome> {
        let mut wait = self.swarm_inbox_wait(actor, run, after, ack)?;
        let start = Instant::now();
        loop {
            if let Some(result) = wait.poll(start.elapsed() >= timeout, &mut check_cancel)? {
                return Ok(result);
            }
            std::thread::sleep(
                timeout
                    .saturating_sub(start.elapsed())
                    .min(Duration::from_millis(250)),
            );
        }
    }
    pub fn swarm_inbox_wait(
        &self,
        actor: &str,
        run: &str,
        after: Option<i64>,
        ack: Option<i64>,
    ) -> Result<SwarmInboxWait<'_>> {
        ensure!(
            !(after.is_some() && ack.is_some())
                && after.is_none_or(|v| v >= 0)
                && ack.is_none_or(|v| v > 0),
            Invalid.error(
                "invalid_cursor",
                "use after to replay or ack with a processed notification cursor"
            )
        );
        let (id, target, last, first_contact) = self.resolve_inbox(actor, run)?;
        Ok(SwarmInboxWait {
            board: self,
            actor: actor.into(),
            run: format!("#{id}"),
            after: after.unwrap_or(last),
            ack,
            target,
            first_contact,
        })
    }
}
impl SwarmInboxWait<'_> {
    pub fn try_poll(&mut self, timed_out: bool) -> Result<Option<SwarmWaitOutcome>> {
        let board = self.board;
        match board.try_without_lock_wait(|| self.poll(timed_out, || Ok(()))) {
            Err(e)
                if timed_out
                    && self.ack.is_none()
                    && crate::errors::classify(&e).0 == crate::errors::Kind::Busy =>
            {
                Ok(Some(SwarmWaitOutcome::StillWaiting { cursor: self.after }))
            }
            result => result,
        }
    }
    pub fn poll(
        &mut self,
        timed_out: bool,
        mut check_cancel: impl FnMut() -> Result<()>,
    ) -> Result<Option<SwarmWaitOutcome>> {
        if let Some(through) = self.ack {
            check_cancel()?;
            self.after =
                self.board
                    .swarm_acknowledge(&self.actor, &self.run, &self.target, through)?;
            self.ack = None;
        }
        let after = self.after;
        loop {
            check_cancel()?;
            let poll = self.board.poll_inbox(
                &self.actor,
                &self.run,
                &self.target,
                after,
                Some(timed_out),
            )?;
            check_cancel()?;
            if self.first_contact
                && (matches!(poll, WaitPoll::Ready { .. })
                    || (!timed_out && matches!(poll, WaitPoll::Pending)))
            {
                self.board.swarm_first_contact(&self.actor, &self.run)?;
                self.first_contact = false;
                continue; // Check for stop, revocation or new messages after the check-in write.
            }
            return Ok(match poll {
                WaitPoll::Stopped { cursor } => Some(SwarmWaitOutcome::Stopped { cursor }),
                WaitPoll::Pending if timed_out => {
                    Some(SwarmWaitOutcome::StillWaiting { cursor: after })
                }
                WaitPoll::Pending => None,
                WaitPoll::Ready {
                    cursor,
                    notifications,
                    messages,
                    dms,
                } => Some(SwarmWaitOutcome::Messages {
                    cursor,
                    notifications,
                    messages,
                    dms,
                }),
            });
        }
    }
}
impl Board {
    // Polling an idle participant must not count the whole feed or load every other brief.
    pub(super) fn swarm_waiting_on(
        &self,
        c: &Connection,
        actor: &str,
        run: &str,
        action: Action,
    ) -> Result<(Access, SwarmRun)> {
        let (a, mut r) = self.swarm_record(c, actor, run, action)?;
        if !r.operator(actor) {
            let sql = format!("{WORKER_SELECT} WHERE w.run_id=? AND w.actor=?");
            r.workers.extend(
                c.query_row(&sql, params![r.id, actor], |s| worker(s, &a))
                    .optional()?,
            );
        }
        Ok((a, r))
    }
    pub fn swarm_wait_target(&self, actor: &str, run: &str) -> Result<(String, i64)> {
        let (_, target, last, _) = self.resolve_inbox(actor, run)?;
        Ok((target, last))
    }
    fn resolve_inbox(&self, actor: &str, run: &str) -> Result<(i64, String, i64, bool)> {
        let tx = self.conn.unchecked_transaction()?;
        let (_, r) = self.swarm_waiting_on(&tx, actor, run, Action::Read)?;
        let out = if r.operator(actor) {
            (
                "coordinator".into(),
                tx.query_row(
                    "SELECT coordinator_cursor FROM swarm_runs WHERE id=?",
                    [r.id],
                    |s| s.get(0),
                )?,
                false,
            )
        } else {
            let w = r.workers.iter().find(|w| w.actor == actor).context(
                Forbidden.error("wrong_swarm_actor", "only participants have a swarm inbox"),
            )?;
            (
                w.name.clone(),
                w.last_read_cursor,
                matches!(w.status.as_str(), "registered" | "launch_failed"),
            )
        };
        tx.commit()?;
        Ok((r.id, out.0, out.1, out.2))
    }
    pub fn swarm_poll_wait(
        &self,
        actor: &str,
        run: &str,
        target: &str,
        after: i64,
    ) -> Result<WaitPoll> {
        self.poll_inbox(actor, run, target, after, None)
    }

    fn poll_inbox(
        &self,
        actor: &str,
        run: &str,
        target: &str,
        after: i64,
        wait_timed_out: Option<bool>,
    ) -> Result<WaitPoll> {
        ensure!(
            after >= 0,
            Invalid.error("invalid_cursor", "cursor cannot be negative")
        );
        let tx = self.conn.unchecked_transaction()?;
        let (a, r) = self.swarm_waiting_on(&tx, actor, run, Action::Read)?;
        let worker = wait_for(&r, actor, target)?;
        if r.status != "active"
            || worker.is_some_and(|w| matches!(w.status.as_str(), "done" | "stopped"))
        {
            let cursor = tx.query_row(
                "SELECT coalesce(stop_cursor,?) FROM swarm_runs WHERE id=?",
                params![after, r.id],
                |s| s.get(0),
            )?;
            tx.commit()?;
            return Ok(WaitPoll::Stopped { cursor });
        }
        let notifications=rows(tx.prepare("SELECT id,run_id,target_worker,source_kind,from_actor,message_id,dm_id,event_cursor,created_at FROM swarm_notifications WHERE run_id=? AND target_worker=? AND event_cursor>?
            AND (?4 IS NULL OR message_id IS NULL OR (SELECT channel FROM messages WHERE id=message_id) IN (SELECT value FROM json_each(?4)))
            ORDER BY event_cursor LIMIT 256")?.query_map(params![r.id,target,after,a.read_channels_json(&tx)?],|s| Ok(SwarmNotification{id:s.get(0)?,run_id:s.get(1)?,target_worker:s.get(2)?,source_kind:s.get(3)?,from_actor:s.get(4)?,message_id:s.get(5)?,dm_id:s.get(6)?,event_cursor:s.get(7)?,created_at:s.get(8)?}))?)?;
        // Inspecting the inbox and an empty zero-time probe are reads; an actual
        // participant wait needs Post, whether or not this worker already checked in.
        if let Some(timed_out) = wait_timed_out
            && (!timed_out || !notifications.is_empty())
        {
            a.require(Action::Post, &r.channel)?;
        }
        let Some(cursor) = notifications.last().map(|n| n.event_cursor) else {
            tx.commit()?;
            return Ok(WaitPoll::Pending);
        };
        let mut messages = vec![];
        let mut dms = vec![];
        for n in &notifications {
            if let Some(id) = n.message_id {
                messages.push(a.view(&tx, Board::get_on(&tx, id)?)?);
            }
            if let Some(id) = n.dm_id {
                dms.push(feed::dm_on(&tx, id)?);
            }
        }
        tx.commit()?;
        Ok(WaitPoll::Ready {
            cursor,
            notifications,
            messages,
            dms,
        })
    }
    fn swarm_acknowledge(&self, actor: &str, run: &str, target: &str, through: i64) -> Result<i64> {
        self.write(|tx| {
            let (a, r) = self.swarm_waiting_on(tx, actor, run, Action::Post)?;
            let w = wait_for(&r, actor, target)?;
            ensure!(
                tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM swarm_notifications WHERE run_id=? AND target_worker=? AND event_cursor=?
                        AND (?4 IS NULL OR message_id IS NULL OR (SELECT channel FROM messages WHERE id=message_id) IN (SELECT value FROM json_each(?4))))",
                    params![r.id, target, through,a.read_channels_json(tx)?],
                    |s| s.get::<_, bool>(0),
                )?,
                Invalid.error("invalid_cursor", "ack must be a notification cursor from your own inbox")
            );
            if let Some(w) = w {
                if through > w.last_read_cursor {
                    tx.execute("UPDATE swarm_workers SET last_read_cursor=? WHERE id=?", params![through,w.id])?;
                }
                Ok(through.max(w.last_read_cursor))
            } else {
                let last: i64 = tx.query_row("SELECT coordinator_cursor FROM swarm_runs WHERE id=?", [r.id], |row| row.get(0))?;
                if through > last {
                    tx.execute("UPDATE swarm_runs SET coordinator_cursor=? WHERE id=?", params![through,r.id])?;
                }
                Ok(through.max(last))
            }
        })
    }
    pub fn swarm_done(&self, actor: &str, run: &str, summary: Option<&str>) -> Result<SwarmWorker> {
        self.write(|tx| {
            let (a, r) = self.swarm_on(tx, actor, run, Action::Post)?;
            let w = r
                .workers
                .iter()
                .find(|w| w.actor == actor)
                .context(Forbidden.error(
                    "wrong_swarm_actor",
                    "only a registered worker can finish its own assignment",
                ))?;
            if w.status == "done" {
                return Ok(w.clone());
            }
            r.active()?;
            if let Some(s) = summary {
                self.swarm_post_on(
                    tx,
                    &a,
                    &r,
                    actor,
                    s,
                    &PostOptions {
                        no_sync: true,
                        ..Default::default()
                    },
                )?;
            }
            tx.execute(
                "UPDATE swarm_workers SET status='done',updated_at=?1,last_activity_at=?1,checked_in_at=coalesce(checked_in_at,?1),launch_error=NULL WHERE id=?2",
                params![crate::now(), w.id],
            )?;
            status_event(tx, &r, actor, &w.name, "done")?;
            let mut result = w.clone();
            result.status = "done".into();
            result.updated_at = tx.query_row(
                "SELECT updated_at FROM swarm_workers WHERE id=?",
                [w.id],
                |s| s.get(0),
            )?;
            result.last_activity_at=Some(result.updated_at.clone());
            if result.checked_in_at.is_none() {result.checked_in_at=Some(result.updated_at.clone());}
            result.launch_error=None;
            Ok(result)
        })
    }
}
