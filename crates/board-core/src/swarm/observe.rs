use super::*;

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct RunAction {
    pub cursor: i64,
    pub kind: String,
    pub assigned_to: String,
    pub summary: String,
    pub message_id: i64,
    pub thread_id: i64,
    pub task_id: Option<i64>,
    pub blocked_dependents: i64,
    pub worker: Option<String>,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct RunChange {
    pub cursor: i64,
    pub kind: String,
    pub message_id: i64,
    pub actor: String,
    pub worker: Option<String>,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct RunThread {
    pub id: i64,
    pub title: String,
    pub state: String,
    pub assignee: Option<String>,
    pub briefed_workers: Vec<String>,
    pub claimant: Option<String>,
    pub latest_handoff: Option<i64>,
    pub latest_check: Option<i64>,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct TaskExplanation {
    pub task_id: i64,
    pub steps: Vec<WaitStep>,
    pub truncated: bool,
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct WaitStep {
    pub id: i64,
    pub title: String,
    pub state: String,
    pub reason: String,
    pub waits_on: Vec<i64>,
    pub handoff: Option<TaskHandoff>,
}
impl WaitStep {
    pub fn review_link(&self) -> Option<(i64, String)> {
        let h = self.handoff.as_ref()?;
        Some(match &h.verification {
            Some(v) => (v.id, format!("Verification #{}: {}", v.id, v.status)),
            None => (
                h.id,
                format!("Handoff #{} awaits verification by @{}", h.id, h.to),
            ),
        })
    }
}
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct SwarmObservation {
    pub run: String,
    pub goal: String,
    pub channel: String,
    pub thread_id: i64,
    pub status: String,
    pub workers: BTreeMap<String, i64>,
    pub tasks: BTreeMap<String, i64>,
    pub threads: Vec<RunThread>,
    pub next_task_after: Option<i64>,
    pub actions: Vec<RunAction>,
    pub next_action_after: Option<i64>,
    pub changes: Vec<RunChange>,
    pub cursor: i64,
    pub has_more: bool,
}

fn counts(
    c: &Connection,
    sql: &str,
    scope: impl rusqlite::Params,
) -> Result<BTreeMap<String, i64>> {
    Ok(c.prepare(sql)?
        .query_map(scope, |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

impl Board {
    /// Start in this run and follow explicit, readable, unfinished prerequisites.
    /// Only this run's evidence is relevant; review never creates a dependency.
    pub fn swarm_explain_task(
        &self,
        actor: &str,
        run: &str,
        task_id: i64,
        limit: usize,
    ) -> Result<TaskExplanation> {
        ensure!(
            (1..=100).contains(&limit),
            Invalid.error("invalid_page", "limit must be 1..100")
        );
        let tx = self.conn.unchecked_transaction()?;
        let (access, r) = self.swarm_record(&tx, actor, run, Action::Read)?;
        let channels = access.read_channels_json(&tx)?;
        let channel: String = tx
            .query_row(
                concat!(
                    include_str!("readable_threads.sql"),
                    "SELECT channel FROM run_threads WHERE id=?3"
                ),
                params![channels, r.id, task_id],
                |s| s.get(0),
            )
            .optional()?
            .context(Missing.error("not_found", "task not found in this run"))?;
        let mut pending = vec![(task_id, channel)];
        let mut seen = BTreeSet::from([task_id]);
        let mut steps = Vec::new();
        let mut task = tx.prepare("SELECT substr(m.text,1,160),
            CASE WHEN t.state='claimed' AND t.lease_until<=unixepoch() THEN 'expired' ELSE t.state END,
            t.holder, substr(t.result,1,400)
            FROM tasks t JOIN messages m USING(id) WHERE t.id=? AND m.channel=? AND NOT m.deleted")?;
        let mut needs = tx.prepare(
            "SELECT n.need,m.channel FROM task_needs n JOIN tasks t ON t.id=n.need
            JOIN messages m ON m.id=n.need WHERE n.task=? AND t.state!='completed'
            AND NOT m.deleted ORDER BY n.need",
        )?;
        while steps.len() < pending.len().min(limit) {
            let (id, channel) = &pending[steps.len()];
            let id = *id;
            let (title, state, holder, result): (String, String, Option<String>, Option<String>) =
                task.query_row(params![id, channel], |s| {
                    Ok((s.get(0)?, s.get(1)?, s.get(2)?, s.get(3)?))
                })
                .optional()?
                .context(Missing.error("not_found", "task not found in this run"))?;
            let mut waits_on = Vec::new();
            if state == "waiting" {
                let mut links = needs.query([id])?;
                while let Some(link) = links.next()? {
                    let channel = link.get_ref(1)?.as_str()?;
                    if access.allows(Action::Read, channel) {
                        let need = link.get(0)?;
                        waits_on.push(need);
                        if seen.insert(need) {
                            pending.push((need, channel.to_owned()));
                        }
                    }
                }
            }
            let reason = match state.as_str() {
                "waiting" if waits_on.is_empty() => "No unfinished prerequisite is visible.".into(),
                "waiting" => format!(
                    "Visible unfinished prerequisites: {}",
                    waits_on
                        .iter()
                        .map(|n| format!("#{n}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "blocked" | "failed" | "canceled" => {
                    result.unwrap_or_else(|| "No reason recorded.".into())
                }
                "ready" => "Ready to claim; no reason for inactivity is recorded.".into(),
                "claimed" => format!(
                    "Claimed by {}; activity inside the harness is unknown.",
                    holder.as_deref().unwrap_or("unknown")
                ),
                "expired" => "Claim lease expired; worker activity is unknown.".into(),
                "completed" => "Task completed.".into(),
                _ => "No reason recorded.".into(),
            };
            steps.push(WaitStep {
                id,
                title: title.lines().next().unwrap_or_default().into(),
                state,
                reason,
                waits_on,
                handoff: task_handoff_on(&tx, channels.as_deref(), id, Some(r.id))?,
            });
        }
        drop((task, needs));
        tx.commit()?;
        Ok(TaskExplanation {
            task_id,
            truncated: steps.len() < pending.len(),
            steps,
        })
    }
    /// A viewer's public feed bookmark is independent of any participant's addressed inbox.
    pub fn swarm_public_seen(&self, actor: &str, run: &str) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        let (a, r) = self.swarm_record(&tx, actor, run, Action::Read)?;
        let at = tx
            .query_row(
                "SELECT through FROM swarm_public_seen WHERE run_id=? AND viewer=?",
                params![r.id, a.private_key(actor)?],
                |s| s.get(0),
            )
            .optional()?
            .unwrap_or(0);
        tx.commit()?;
        Ok(at)
    }
    pub fn swarm_mark_public_seen(&self, actor: &str, run: &str, through: i64) -> Result<()> {
        self.write(|tx| {
            let (a, r) = self.swarm_record(tx, actor, run, Action::Read)?;
            ensure!(tx.query_row(concat!(include_str!("readable_threads.sql"),
                "SELECT EXISTS(SELECT 1 FROM messages m JOIN run_threads s ON s.id=coalesce(m.re,m.id) WHERE m.id=?3)"),
                params![a.read_channels_json(tx)?, r.id, through], |s| s.get::<_,bool>(0))?,
                Invalid.error("invalid_cursor","expected a public message from this run"));
            tx.execute("INSERT INTO swarm_public_seen VALUES(?,?,?) ON CONFLICT(run_id,viewer) DO UPDATE SET through=max(through,excluded.through)",
                params![r.id, a.private_key(actor)?, through])?;
            Ok(())
        })
    }
    /// Current exceptions and chronological deltas are separate pages from one
    /// snapshot. Neither replays prose, loads briefs, nor exposes another run's tasks.
    pub fn swarm_observe(
        &self,
        actor: &str,
        run: &str,
        after: Option<i64>,
        action_after: i64,
        limit: usize,
    ) -> Result<SwarmObservation> {
        self.swarm_observe_page(actor, run, after, action_after, 0, limit)
    }
    pub fn swarm_observe_page(
        &self,
        actor: &str,
        run: &str,
        after: Option<i64>,
        action_after: i64,
        task_after: i64,
        limit: usize,
    ) -> Result<SwarmObservation> {
        ensure!(
            (1..=100).contains(&limit)
                && after.unwrap_or(0) >= 0
                && action_after >= 0
                && task_after >= 0,
            Invalid.error(
                "invalid_page",
                "limit must be 1..100 and positions nonnegative"
            )
        );
        let tx = self.conn.unchecked_transaction()?;
        let (access, r) = self.swarm_record(&tx, actor, run, Action::Read)?;
        let channels = access.read_channels_json(&tx)?;
        let workers = counts(
            &tx,
            "SELECT status,count(*) FROM swarm_workers WHERE run_id=? GROUP BY status",
            [r.id],
        )?;
        let tasks = counts(
            &tx,
            concat!(
                include_str!("readable_threads.sql"),
                "SELECT CASE WHEN t.state='claimed' AND t.lease_until<=unixepoch() THEN 'expired' ELSE t.state END AS effective,count(*) FROM run_threads JOIN tasks t USING(id) GROUP BY effective"
            ),
            params![channels, r.id],
        )?;
        let fetch = (limit + 1) as i64;
        let mut threads = rows(
            tx.prepare(concat!(
                include_str!("readable_threads.sql"),
                ", selected_runs AS (SELECT ?2 AS id)",
                include_str!("readable_evidence.sql"),
                include_str!("observation_threads.sql")
            ))?
            .query_map(params![channels, r.id, task_after, fetch], |s| {
                Ok(RunThread {
                    id: s.get(0)?,
                    title: s
                        .get::<_, String>(1)?
                        .lines()
                        .next()
                        .unwrap_or("")
                        .to_owned(),
                    state: s.get(2)?,
                    assignee: s.get(3)?,
                    claimant: s.get(4)?,
                    latest_handoff: s.get(5)?,
                    latest_check: s.get(6)?,
                    briefed_workers: serde_json::from_str(s.get_ref(7)?.as_str()?).map_err(
                        |e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                7,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        },
                    )?,
                })
            })?,
        )?;
        let next_task_after = (threads.len() > limit).then(|| threads[limit - 1].id);
        threads.truncate(limit);
        let mut actions = rows(
            tx.prepare(concat!(
                include_str!("readable_threads.sql"),
                ", selected_runs AS (SELECT id,slug,root_id,status,coordinator_actor FROM swarm_runs WHERE id=?2)",
                include_str!("readable_evidence.sql"),
                include_str!("current_run_actions.sql"),
                include_str!("observation_actions.sql")
            ))?
                .query_map(
                    params![
                        channels,
                        r.id,
                        action_after,
                        fetch
                    ],
                    |s| {
                        Ok(RunAction {
                            cursor: s.get(0)?,
                            kind: s.get(1)?,
                            assigned_to: s.get(2)?,
                            summary: s.get(3)?,
                            message_id: s.get(4)?,
                            task_id: s.get(5)?,
                            blocked_dependents: s.get(6)?,
                            worker: s.get(7)?,
                            thread_id: s.get(8)?,
                        })
                    },
                )?,
        )?;
        let next_action_after = (actions.len() > limit).then(|| actions[limit - 1].cursor);
        actions.truncate(limit);
        // A first observation gives recent context; an explicit cursor delivers every
        // subsequent change oldest first. A newer unrelated event never advances it.
        let order = if after.is_some() { "ASC" } else { "DESC" };
        let sql=format!(concat!(include_str!("readable_threads.sql"),
          ", selected_runs AS (SELECT ?2 AS id)", include_str!("readable_evidence.sql"),
          " SELECT e.cursor,coalesce(v.kind,e.kind),e.message_id,e.actor,json_extract(e.payload,'$.worker')
          FROM events e LEFT JOIN readable_evidence v ON v.message_id=e.cursor AND e.kind='message.posted'
          WHERE e.cursor>?3 AND ((e.kind GLOB 'task.*' AND e.message_id IN (SELECT id FROM run_threads))
            OR (e.kind IN ('swarm.started','swarm.registered','swarm.checked_in','swarm.launch_failed','swarm.status','swarm.stopped','swarm.limit','swarm.budget_reached') AND json_extract(e.payload,'$.run')=?4)
            OR v.message_id IS NOT NULL)
          ORDER BY e.cursor {} LIMIT ?5"), order);
        let mut changes = rows(tx.prepare(&sql)?.query_map(
            params![channels, r.id, after.unwrap_or(0), r.slug, fetch],
            |s| {
                Ok(RunChange {
                    cursor: s.get(0)?,
                    kind: s.get(1)?,
                    message_id: s.get(2)?,
                    actor: s.get(3)?,
                    worker: s.get(4)?,
                })
            },
        )?)?;
        let has_more = after.is_some() && changes.len() > limit;
        changes.truncate(limit);
        if after.is_none() {
            changes.reverse();
        }
        let cursor = changes
            .last()
            .map(|e| e.cursor)
            .unwrap_or(after.unwrap_or(0));
        tx.commit()?;
        Ok(SwarmObservation {
            run: r.slug,
            goal: r.goal,
            channel: r.channel,
            thread_id: r.thread_id,
            status: r.status,
            workers,
            tasks,
            threads,
            next_task_after,
            actions,
            next_action_after,
            changes,
            cursor,
            has_more,
        })
    }
}
