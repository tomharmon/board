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

struct WaitRegistration<'a> {
    board: &'a Board,
    actor: &'a str,
    run: &'a str,
    target: String,
    pid: u32,
    active: bool,
}
impl WaitRegistration<'_> {
    fn enter(&mut self) -> Result<()> {
        self.board.swarm_set_worker_status(
            self.actor,
            self.run,
            &self.target,
            "waiting",
            Some(self.pid),
        )?;
        self.active = true;
        Ok(())
    }
    fn leave(&mut self, through: Option<i64>) -> Result<()> {
        if self.active {
            self.board
                .swarm_leave_wait(self.actor, self.run, &self.target, self.pid, through)?;
            self.active = false;
        }
        Ok(())
    }
}
impl Drop for WaitRegistration<'_> {
    fn drop(&mut self) {
        let _ = self.leave(None);
    }
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
        Some(r.root_id),
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
        timeout: Duration,
        mut check_cancel: impl FnMut() -> Result<()>,
    ) -> Result<SwarmWaitOutcome> {
        let (target, last) = self.swarm_wait_target(actor, run)?;
        let after = after.unwrap_or(last);
        let mut registration = WaitRegistration {
            board: self,
            actor,
            run,
            target,
            pid: std::process::id(),
            active: false,
        };
        let start = Instant::now();
        loop {
            check_cancel()?;
            let poll = self.swarm_poll_wait(actor, run, &registration.target, after)?;
            match poll {
                WaitPoll::Stopped { cursor } => {
                    registration.leave(None)?;
                    return Ok(SwarmWaitOutcome::Stopped { cursor });
                }
                WaitPoll::Pending if start.elapsed() >= timeout => {
                    // In particular, an empty zero-duration probe never acquires a write transaction.
                    registration.leave(None)?;
                    return Ok(SwarmWaitOutcome::StillWaiting { cursor: after });
                }
                WaitPoll::Ready {
                    cursor,
                    notifications,
                    messages,
                    dms,
                } => {
                    check_cancel()?;
                    if registration.active {
                        registration.leave(Some(cursor))?;
                    } else if !self.swarm_ack_ready(actor, run, &registration.target, cursor)? {
                        continue;
                    }
                    return Ok(SwarmWaitOutcome::Messages {
                        cursor,
                        notifications,
                        messages,
                        dms,
                    });
                }
                WaitPoll::Pending if !registration.active => {
                    if let Err(e) = registration.enter() {
                        if let WaitPoll::Stopped { cursor } =
                            self.swarm_poll_wait(actor, run, &registration.target, after)?
                        {
                            return Ok(SwarmWaitOutcome::Stopped { cursor });
                        }
                        return Err(e);
                    }
                    // Re-read after registration: a stop or a new notification can win that gap.
                }
                WaitPoll::Pending => {
                    std::thread::sleep(
                        timeout
                            .saturating_sub(start.elapsed())
                            .min(Duration::from_millis(250)),
                    );
                }
            }
        }
    }
    // Polling an idle participant must not count the whole feed or load every other brief.
    fn swarm_waiting_on(
        &self,
        c: &Connection,
        actor: &str,
        run: &str,
        action: Action,
    ) -> Result<(Access, SwarmRun)> {
        let (a, mut r) = self.swarm_record(c, actor, run, action)?;
        if !r.operator(actor) {
            let sql = "SELECT id,run_id,name,actor,role_slug,pinned_role_version,brief,workspace_path,branch_name,status,waiter_pid,last_read_cursor,created_at,updated_at FROM swarm_workers WHERE run_id=? AND actor=?";
            r.workers
                .extend(c.query_row(sql, params![r.id, actor], worker).optional()?);
        }
        Ok((a, r))
    }
    pub fn swarm_wait_target(&self, actor: &str, run: &str) -> Result<(String, i64)> {
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
            )
        } else {
            let w = r.workers.iter().find(|w| w.actor == actor).context(
                Forbidden.error("wrong_swarm_actor", "only participants have a swarm inbox"),
            )?;
            (w.name.clone(), w.last_read_cursor)
        };
        tx.commit()?;
        Ok(out)
    }
    pub fn swarm_poll_wait(
        &self,
        actor: &str,
        run: &str,
        target: &str,
        after: i64,
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
        let notifications=rows(tx.prepare("SELECT id,run_id,target_worker,source_kind,from_actor,message_id,dm_id,event_cursor,created_at FROM swarm_notifications WHERE run_id=? AND target_worker=? AND event_cursor>? ORDER BY event_cursor LIMIT 256")?.query_map(params![r.id,target,after],|s| Ok(SwarmNotification{id:s.get(0)?,run_id:s.get(1)?,target_worker:s.get(2)?,source_kind:s.get(3)?,from_actor:s.get(4)?,message_id:s.get(5)?,dm_id:s.get(6)?,event_cursor:s.get(7)?,created_at:s.get(8)?}))?)?;
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
    pub fn swarm_set_worker_status(
        &self,
        actor: &str,
        run: &str,
        target: &str,
        status: &str,
        waiter_pid: Option<u32>,
    ) -> Result<()> {
        ensure!(
            matches!(status, "working" | "waiting" | "done")
                && (status == "waiting") == waiter_pid.is_some()
                && waiter_pid.is_none_or(|pid| (1..=i32::MAX as u32).contains(&pid)),
            Invalid.error(
                "invalid_worker_status",
                "waiting requires a positive process ID"
            )
        );
        self.write(|tx|{
            let (_,r)=self.swarm_on(tx,actor,run,Action::Post)?;
            let w=wait_for(&r,actor,target)?;
            // Stop between reading status and entering wait must still produce a stopped response.
            if r.status!="active"{return Ok(());}
            if let Some(w)=w {
                if matches!(w.status.as_str(),"done"|"stopped"){return Ok(());}
                if w.status!=status || w.waiter_pid!=waiter_pid {
                    tx.execute("UPDATE swarm_workers SET status=?,waiter_pid=?,updated_at=? WHERE id=?",params![status,waiter_pid,crate::now(),w.id])?;
                    status_event(tx,&r,actor,target,status)?;
                }
            }else{
                ensure!(status!="done",Invalid.error("invalid_worker_status","stop the run instead of completing the coordinator"));
                tx.execute("UPDATE swarm_runs SET coordinator_pid=? WHERE id=? AND coordinator_pid IS NOT ?",params![waiter_pid,r.id,waiter_pid])?;
            }Ok(())
        })
    }
    fn swarm_ack_ready(&self, actor: &str, run: &str, target: &str, through: i64) -> Result<bool> {
        self.write(|tx| {
            let (_, r) = self.swarm_on(tx, actor, run, Action::Post)?;
            let w = wait_for(&r, actor, target)?;
            if r.status != "active"
                || w.is_some_and(|w| matches!(w.status.as_str(), "done" | "stopped"))
            {
                return Ok(false);
            }
            ensure!(
                tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM swarm_notifications WHERE run_id=? AND target_worker=? AND event_cursor=?)",
                    params![r.id, target, through],
                    |s| s.get::<_, bool>(0),
                )?,
                Invalid.error("invalid_cursor", "expected a delivered notification cursor")
            );
            if let Some(w) = w {
                let was_waiting = w.status != "working";
                if was_waiting || w.waiter_pid.is_some() || w.last_read_cursor < through {
                    tx.execute(
                        "UPDATE swarm_workers SET status='working',waiter_pid=NULL,updated_at=?,last_read_cursor=max(last_read_cursor,?) WHERE id=?",
                        params![crate::now(), through, w.id],
                    )?;
                }
                if was_waiting {
                    status_event(tx, &r, actor, target, "working")?;
                }
            } else {
                tx.execute(
                    "UPDATE swarm_runs SET coordinator_cursor=max(coordinator_cursor,?),coordinator_pid=NULL WHERE id=?",
                    params![through, r.id],
                )?;
            }
            Ok(true)
        })
    }
    pub fn swarm_leave_wait(
        &self,
        actor: &str,
        run: &str,
        target: &str,
        pid: u32,
        through: Option<i64>,
    ) -> Result<()> {
        self.write(|tx|{let (_,r)=self.swarm_on(tx,actor,run,Action::Post)?;
            let w=wait_for(&r,actor,target)?;
            if r.status!="active"{return Ok(());}
            if let Some(position)=through {
                ensure!(tx.query_row("SELECT EXISTS(SELECT 1 FROM swarm_notifications WHERE run_id=? AND target_worker=? AND event_cursor=?)",params![r.id,target,position],|s|s.get::<_,bool>(0))?,
                        Invalid.error("invalid_cursor","expected a delivered notification cursor"));
            }
            if let Some(w)=w {
                if w.status=="waiting" && w.waiter_pid==Some(pid) {
                    tx.execute("UPDATE swarm_workers SET status='working',waiter_pid=NULL,updated_at=?,last_read_cursor=max(last_read_cursor,?) WHERE id=?",params![crate::now(),through.unwrap_or(0),w.id])?;
                    status_event(tx,&r,actor,target,"working")?;
                }
            }else{
                tx.execute("UPDATE swarm_runs SET coordinator_cursor=max(coordinator_cursor,?),coordinator_pid=NULL WHERE id=? AND coordinator_pid=?",params![through.unwrap_or(0),r.id,pid])?;
            }Ok(())})
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
                "UPDATE swarm_workers SET status='done',waiter_pid=NULL,updated_at=? WHERE id=?",
                params![crate::now(), w.id],
            )?;
            status_event(tx, &r, actor, &w.name, "done")?;
            let mut result = w.clone();
            result.status = "done".into();
            result.waiter_pid = None;
            result.updated_at = tx.query_row(
                "SELECT updated_at FROM swarm_workers WHERE id=?",
                [w.id],
                |s| s.get(0),
            )?;
            Ok(result)
        })
    }
}
