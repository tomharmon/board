use super::*;

#[derive(Debug, Serialize)]
pub struct RunAction {
    pub cursor: i64,
    pub kind: String,
    pub assigned_to: String,
    pub summary: String,
    pub message_id: i64,
    pub task_id: Option<i64>,
    pub blocked_dependents: i64,
}
#[derive(Debug, Serialize)]
pub struct RunChange {
    pub cursor: i64,
    pub kind: String,
    pub message_id: i64,
    pub actor: String,
    pub worker: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct SwarmObservation {
    pub run: String,
    pub status: String,
    pub policy: RunPolicy,
    pub available_capacity: i64,
    pub workers: BTreeMap<String, i64>,
    pub tasks: BTreeMap<String, i64>,
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
        ensure!(
            (1..=100).contains(&limit) && after.unwrap_or(0) >= 0 && action_after >= 0,
            Invalid.error(
                "invalid_page",
                "limit must be 1..100 and positions nonnegative"
            )
        );
        let tx = self.conn.unchecked_transaction()?;
        let (_, r) = self.swarm_record(&tx, actor, run, Action::Read)?;
        let policy = RunPolicy::read(&tx, r.id)?;
        let workers = counts(
            &tx,
            "SELECT status,count(*) FROM swarm_workers WHERE run_id=? GROUP BY status",
            [r.id],
        )?;
        let tasks = counts(
            &tx,
            "SELECT CASE WHEN t.state='claimed' AND t.lease_until<=unixepoch() THEN 'expired' ELSE t.state END AS effective,count(*) FROM messages m JOIN tasks t USING(id) WHERE m.channel=? AND NOT m.deleted GROUP BY effective",
            [&r.channel],
        )?;
        let occupied = workers.get("working").copied().unwrap_or(0)
            + workers.get("waiting").copied().unwrap_or(0);
        let capacity = if r.status == "active" {
            (policy.max_active - occupied).max(0)
        } else {
            0
        };
        let fetch = (limit + 1) as i64;
        let mut actions = rows(
            tx.prepare(include_str!("observation_actions.sql"))?
                .query_map(
                    params![
                        r.id,
                        r.channel,
                        r.coordinator_actor,
                        r.root_id,
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
                        })
                    },
                )?,
        )?;
        let next_action_after = (actions.len() > limit).then(|| actions[limit - 1].cursor);
        actions.truncate(limit);
        // A first observation gives recent context; an explicit cursor delivers every
        // subsequent change oldest first. A newer unrelated event never advances it.
        let order = if after.is_some() { "ASC" } else { "DESC" };
        let sql=format!("SELECT e.cursor,CASE WHEN e.kind='message.posted' THEN coalesce(v.kind,'swarm.started') ELSE e.kind END,e.message_id,e.actor,json_extract(e.payload,'$.worker')
          FROM events e JOIN messages m ON m.id=e.message_id LEFT JOIN swarm_evidence v ON v.message_id=e.cursor AND v.run_id=?1
          WHERE m.channel=?2 AND e.cursor>?3 AND (e.kind GLOB 'task.*' OR e.kind IN ('swarm.registered','swarm.checked_in','swarm.launch_failed','swarm.status','swarm.stopped','swarm.limit','swarm.budget_reached') OR (e.kind='message.posted' AND (v.message_id IS NOT NULL OR e.message_id=?4)))
          ORDER BY e.cursor {order} LIMIT ?5");
        let mut changes = rows(tx.prepare(&sql)?.query_map(
            params![r.id, r.channel, after.unwrap_or(0), r.root_id, fetch],
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
            status: r.status,
            policy,
            available_capacity: capacity,
            workers,
            tasks,
            actions,
            next_action_after,
            changes,
            cursor,
            has_more,
        })
    }
}
