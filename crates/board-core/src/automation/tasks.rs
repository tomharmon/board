use super::{now, write};
use crate::access::{Access, Action};
use crate::core::fingerprint;
use crate::errors::Kind::{Conflict, Forbidden, Invalid, Missing};
use crate::swarm::WorkerBinding;
use crate::{Board, PostOptions};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Task {
    pub id: i64,
    pub channel: String,
    pub text: String,
    pub owner: String,
    /// Exact actor that adopted or created the task; the owner is its person.
    #[serde(default)]
    pub by: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<crate::MentionResolution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replies: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<TaskReply>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_names: Vec<String>,
    pub assignee: Option<String>,
    pub state: String,
    pub holder: Option<String>,
    pub epoch: i64,
    /// Only the current claim/recovery/renewal response carries its private proof.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_token: Option<String>,
    pub lease_until: Option<i64>,
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_kind: Option<String>,
    pub needs: Vec<i64>,
    /// Expanded only when opening the task or receiving its lease, never in event snapshots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<TaskInput>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct TaskInput {
    /// The prerequisite's ordinary task thread is the source of its state and result.
    pub id: i64,
    pub state: String,
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<crate::swarm::TaskHandoff>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct TaskReply {
    pub id: i64,
    pub actor: String,
    pub text: String,
}

fn ids(conn: &Connection, sql: &str, p: impl rusqlite::Params) -> Result<Vec<i64>> {
    Ok(conn
        .prepare(sql)?
        .query_map(p, |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn task_optional(conn: &Connection, id: i64) -> Result<Option<Task>> {
    let needs = ids(
        conn,
        "SELECT need FROM task_needs WHERE task=? ORDER BY rowid",
        [id],
    )?;
    Ok(conn
        .query_row(
            "SELECT channel,text,owner,assignee,state,holder,epoch,lease_until,result,
            CASE WHEN state='blocked' THEN (SELECT kind FROM task_holds WHERE task=tasks.id) END
        FROM tasks JOIN messages USING(id) WHERE id=?",
            [id],
            |r| {
                let by: String = r.get(2)?;
                Ok(Task {
                    id,
                    channel: r.get(0)?,
                    text: r.get(1)?,
                    owner: person(&by).to_owned(),
                    by,
                    mentions: vec![],
                    replies: None,
                    last: None,
                    created_at: None,
                    attachment_names: vec![],
                    assignee: r.get(3)?,
                    state: r.get(4)?,
                    holder: r.get(5)?,
                    epoch: r.get(6)?,
                    claim_token: None,
                    lease_until: r.get(7)?,
                    result: r.get(8)?,
                    hold_kind: r.get(9)?,
                    needs,
                    inputs: vec![],
                })
            },
        )
        .optional()?)
}

fn task(conn: &Connection, id: i64) -> Result<Task> {
    task_optional(conn, id)?
        .with_context(|| Missing.error("task_not_found", format!("task #{id} not found")))
}

fn visible(a: &Access, c: &Connection, mut t: Task) -> Result<Task> {
    a.require(Action::Read, &t.channel)?;
    if !a.everywhere(Action::Read) {
        let mut needs = Vec::new();
        for id in t.needs {
            let ch: String = c.query_row("SELECT channel FROM messages WHERE id=?", [id], |r| {
                r.get(0)
            })?;
            if a.allows(Action::Read, &ch) {
                needs.push(id);
            }
        }
        t.needs = needs;
    }
    if t.claim_token.is_some() {
        t.load_inputs(c, a)?;
    }
    Ok(t)
}

impl Task {
    pub(crate) fn load_inputs(&mut self, c: &Connection, a: &Access) -> Result<()> {
        self.inputs.clear();
        if self.needs.is_empty() {
            return Ok(());
        }
        let channels = a.read_channels_json(c)?;
        let mut stmt = c.prepare_cached(
            "SELECT m.channel,
             CASE WHEN t.state='claimed' AND t.lease_until<=unixepoch() THEN 'expired' ELSE t.state END,
             CASE WHEN NOT m.deleted THEN t.result END
             FROM tasks t JOIN messages m USING(id) WHERE t.id=?",
        )?;
        for &id in &self.needs {
            let (channel, state, result): (String, String, Option<String>) =
                stmt.query_row([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            if a.allows(Action::Read, &channel) {
                self.inputs.push(TaskInput {
                    id,
                    state,
                    result,
                    handoff: crate::swarm::task_handoff_on(c, channels.as_deref(), id, None)?,
                });
            }
        }
        Ok(())
    }
}
fn visible_needs(a: &Access, c: &Connection, needs: &[i64]) -> Result<()> {
    for &id in needs {
        a.message(c, id, Action::Read)
            .map_err(|_| Invalid.error("invalid_needs", "prerequisite is not a readable task"))?;
    }
    Ok(())
}

fn remember_creation(c: &Connection, a: &Access, actor: &str, t: &Task) -> Result<()> {
    if !a.allows(Action::Read, &t.channel) {
        c.execute(
            "INSERT INTO blind_tasks VALUES(?,?,?)",
            params![t.id, actor, serde_json::to_string(t)?],
        )?;
        c.execute("INSERT OR IGNORE INTO blind_posts(id) VALUES(?)", [t.id])?;
    }
    Ok(())
}

// A post-only writer gets its own immutable acknowledgement, never a later claim or edit.
fn creation_view(c: &Connection, a: &Access, actor: &str, t: Task) -> Result<Task> {
    if a.allows(Action::Read, &t.channel) {
        return visible(a, c, t);
    }
    a.require(Action::Post, &t.channel)?;
    let data: String = c
        .query_row(
            "SELECT original FROM blind_tasks WHERE task=? AND actor=?",
            params![t.id, actor],
            |r| r.get(0),
        )
        .optional()?
        .context(Conflict.error(
            "duplicate_id",
            "key is not a blind task receipt of this actor",
        ))?;
    Ok(serde_json::from_str(&data)?)
}

pub fn get_task(board: &Board, id: i64) -> Result<Task> {
    let tx = write(&board.conn)?;
    let a = board.access_on(&tx)?;
    a.message(&tx, id, Action::Read)?;
    expire_one(&tx, id, now())?;
    let mut current = visible(&a, &tx, task(&tx, id)?)?;
    current.load_inputs(&tx, &a)?;
    tx.commit()?;
    Ok(current)
}

fn ready_state(conn: &Connection, id: i64) -> Result<&'static str> {
    let waits: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_needs n JOIN tasks t
        ON t.id=n.need WHERE n.task=? AND t.state!='completed')",
        [id],
        |r| r.get(0),
    )?;
    Ok(if waits { "waiting" } else { "ready" })
}

fn publish(tx: &Transaction<'_>, id: i64, actor: &str, action: &str) -> Result<Task> {
    let current = task(tx, id)?;
    let before = Board::get_on(tx, id)?;
    let cursor = Board::event_tx(
        tx,
        &format!("task.{action}"),
        actor,
        Some(id),
        json!({ "task": current }),
    )?;
    let after = Board::get_on(tx, id)?;
    Board::snapshot(tx, cursor, &after, Some(&before))?;
    Ok(current)
}

fn set_state(tx: &Transaction<'_>, id: i64, state: &str, result: Option<&str>) -> Result<()> {
    tx.execute("DELETE FROM task_holds WHERE task=?", [id])?;
    tx.execute("DELETE FROM task_claim_keys WHERE task=?", [id])?;
    tx.execute(
        "UPDATE tasks SET state=?,result=?,holder=NULL,lease_until=NULL WHERE id=?",
        params![state, result, id],
    )?;
    Ok(())
}

fn initial_readiness(needs: &[i64]) -> &'static str {
    if needs.is_empty() { "ready" } else { "waiting" }
}

// A transition with prerequisites first records waiting. Already-completed prerequisites
// make it ready in the same transaction, with a distinct readiness event.
fn publish_transition(tx: &Transaction<'_>, id: i64, actor: &str, action: &str) -> Result<Task> {
    let changed = publish(tx, id, actor, action)?;
    if changed.state == "waiting" && ready_state(tx, id)? == "ready" {
        set_state(tx, id, "ready", None)?;
        publish(tx, id, actor, "ready")
    } else {
        Ok(changed)
    }
}

pub fn expire_leases(board: &Board) -> Result<usize> {
    let tx = write(&board.conn)?;
    let a = board.access_on(&tx)?;
    a.trusted()?;
    let expired = expire(&tx, now(), &a)?;
    tx.commit()?;
    Ok(expired)
}

fn expire(tx: &Transaction<'_>, at: i64, a: &Access) -> Result<usize> {
    let scope = a
        .channels(tx, Action::Read)?
        .map(|v| serde_json::to_string(&v))
        .transpose()?;
    let rows = ids(
        tx,
        "SELECT t.id FROM tasks t JOIN messages m USING(id) WHERE state='claimed' AND lease_until<=?1 AND (?2 IS NULL OR m.channel IN (SELECT value FROM json_each(?2)))",
        params![at, scope],
    )?;
    for &id in &rows {
        set_state(tx, id, ready_state(tx, id)?, None)?;
        publish(tx, id, "board", "expired")?;
    }
    Ok(rows.len())
}

fn expire_one(tx: &Transaction<'_>, id: i64, at: i64) -> Result<()> {
    let elapsed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM tasks WHERE id=? AND state='claimed' AND lease_until<=?)",
        [id, at],
        |r| r.get(0),
    )?;
    if elapsed {
        set_state(tx, id, ready_state(tx, id)?, None)?;
        publish(tx, id, "board", "expired")?;
    }
    Ok(())
}

fn distinct_needs(needs: &[i64]) -> Result<Vec<i64>> {
    let mut seen = std::collections::BTreeSet::new();
    let out: Vec<_> = needs.iter().copied().filter(|n| seen.insert(*n)).collect();
    ensure!(
        out.len() <= 20,
        Invalid.error("invalid_needs", "at most 20 direct prerequisites")
    );
    Ok(out)
}
fn validate_needs(tx: &Transaction<'_>, id: i64, needs: &[i64]) -> Result<()> {
    for &need in needs {
        let state: Option<String> = tx
            .query_row("SELECT state FROM tasks WHERE id=?", [need], |r| r.get(0))
            .optional()?;
        ensure!(
            state.as_deref().is_some_and(|s| s != "canceled"),
            Invalid.error(
                "invalid_needs",
                format!("invalid prerequisite: task #{need} is missing or canceled")
            )
        );
        ensure!(
            need != id,
            Invalid.error("invalid_needs", "task dependency cycle")
        );
    }
    Ok(())
}
fn replace_needs(tx: &Transaction<'_>, id: i64, needs: &[i64]) -> Result<()> {
    validate_needs(tx, id, needs)?;
    tx.execute("DELETE FROM task_needs WHERE task=?", [id])?;
    for need in needs {
        tx.execute("INSERT INTO task_needs(task,need) VALUES(?,?)", [id, *need])?;
    }
    Ok(())
}

fn check_added(c: &Connection, a: &Access, t: &Task, needs: &[i64]) -> Result<()> {
    let added: Vec<_> = needs.iter().filter(|id| !t.needs.contains(id)).collect();
    if added.is_empty() {
        return Ok(());
    }
    let channels = a
        .channels(c, Action::Read)?
        .map(|v| serde_json::to_string(&v))
        .transpose()?;
    let (n,cycle):(i64,bool)=c.query_row("WITH RECURSIVE seen(id) AS (
        SELECT value FROM json_each(?1) UNION SELECT n.need FROM seen s JOIN tasks t ON t.id=s.id
        JOIN messages m ON m.id=s.id JOIN task_needs n ON n.task=s.id WHERE
        t.state!='completed' OR (?2 IS NOT NULL AND m.channel NOT IN (SELECT value FROM json_each(?2))) LIMIT 501)
        SELECT count(*),coalesce(max(id=?3),0) FROM seen",params![serde_json::to_string(&added)?,channels,t.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    ensure!(
        n <= 500,
        Invalid.error(
            "invalid_needs",
            "more than 500 tasks behind new prerequisites; too many to check"
        )
    );
    ensure!(
        !cycle,
        Invalid.error("invalid_needs", "task dependency cycle")
    );
    Ok(())
}

pub enum TaskTarget<'a> {
    New {
        channel: &'a str,
        text: &'a str,
        options: &'a PostOptions,
    },
    Existing(i64),
}

fn person(actor: &str) -> &str {
    actor.split_once('/').map_or(actor, |(p, _)| p)
}

fn validate_assignee(owner: &str, assignee: &str) -> Result<()> {
    crate::validate_actor(assignee)?;
    ensure!(
        assignee
            .split_once('/')
            .is_some_and(|(p, profile)| p == person(owner)
                && profile.len() <= 32
                && profile
                    .bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_lowercase())
                && profile
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')),
        Invalid.error(
            "invalid_assignee",
            "assignee must be a profile of the task owner"
        )
    );
    Ok(())
}

pub fn create_task(
    board: &Board,
    actor: &str,
    target: TaskTarget<'_>,
    needs: &[i64],
    assignee: Option<&str>,
) -> Result<Task> {
    Ok(create_task_with_outcome(board, actor, target, needs, assignee)?.0)
}

pub fn create_task_with_outcome(
    board: &Board,
    actor: &str,
    target: TaskTarget<'_>,
    needs: &[i64],
    assignee: Option<&str>,
) -> Result<(Task, bool)> {
    create_task_and_claim(board, actor, target, needs, assignee, None)
}

pub struct ClaimRequest<'a> {
    pub lease_seconds: i64,
    pub token: &'a str,
}

/// Creation, dependency readiness and the optional first lease have one commit point.
pub fn create_task_and_claim(
    board: &Board,
    actor: &str,
    target: TaskTarget<'_>,
    needs: &[i64],
    assignee: Option<&str>,
    claim: Option<ClaimRequest<'_>>,
) -> Result<(Task, bool)> {
    create_task_in_assignment(board, actor, target, needs, assignee, claim, None)
}

pub fn create_task_in_assignment(
    board: &Board,
    actor: &str,
    target: TaskTarget<'_>,
    needs: &[i64],
    assignee: Option<&str>,
    claim: Option<ClaimRequest<'_>>,
    worker: Option<&WorkerBinding>,
) -> Result<(Task, bool)> {
    create_task_inner(
        board,
        actor,
        target,
        needs,
        assignee,
        Submission::Direct { claim, worker },
    )
}

enum Submission<'a> {
    Direct {
        claim: Option<ClaimRequest<'a>>,
        worker: Option<&'a WorkerBinding>,
    },
    Draft(&'a str),
}
/// Publish exactly the composing actor's saved task entry or leave it intact.
pub fn create_task_draft(
    board: &Board,
    actor: &str,
    target: TaskTarget<'_>,
    needs: &[i64],
    assignee: Option<&str>,
    stamp: &str,
) -> Result<Task> {
    Ok(create_task_inner(
        board,
        actor,
        target,
        needs,
        assignee,
        Submission::Draft(stamp),
    )?
    .0)
}

fn create_task_inner(
    board: &Board,
    actor: &str,
    target: TaskTarget<'_>,
    needs: &[i64],
    assignee: Option<&str>,
    submission: Submission<'_>,
) -> Result<(Task, bool)> {
    crate::validate_actor(actor)?;
    if let Some(who) = assignee {
        validate_assignee(actor, who)?;
    }
    let tx = write(&board.conn)?;
    let at = now();
    let access = board.access_on(&tx)?;
    access.actor(actor)?;
    let (claim, draft) = match submission {
        Submission::Direct { claim, worker } => {
            if claim.is_some()
                && let Some(w) = worker
            {
                board.require_active_worker(&tx, actor, w)?;
            }
            (claim, None)
        }
        Submission::Draft(stamp) => (None, Some(stamp)),
    };
    if let Some(stamp) = draft {
        let mismatch = || {
            Conflict.error(
                "draft_changed",
                "task draft was already sent, edited, or moved",
            )
        };
        let TaskTarget::New {
            channel,
            text,
            options,
        } = &target
        else {
            return Err(mismatch().into());
        };
        let saved = board.check_composer_options_on(&tx, actor, "tasks", text, options)?;
        let route = board
            .composer_settings_on(&tx, actor, "tasks")?
            .task
            .context(mismatch())?;
        let who = route.assignee.trim();
        ensure!(
            saved.channel == *channel
                && saved.reply_to.is_none()
                && route.parsed_needs()? == needs
                && (!who.is_empty()).then_some(who) == assignee,
            mismatch()
        );
        board.consume_composer_on(&tx, actor, "tasks", stamp)?;
    }
    let channel = match &target {
        TaskTarget::New {
            channel, options, ..
        } => {
            access.files(&tx, actor, &options.attachment_ids)?;
            access.require(Action::Post, channel)?;
            (*channel).to_owned()
        }
        TaskTarget::Existing(id) => access.message(&tx, *id, Action::Task)?,
    };
    if claim.is_some() {
        access.require(Action::Read, &channel)?;
        access.require(Action::Task, &channel)?;
    }
    let distinct = distinct_needs(needs)?;
    let needs = distinct.as_slice();
    visible_needs(&access, &tx, needs)?;
    if let Some(request) = &claim {
        validate_claim(request)?;
    }
    let mut mentions = vec![];
    let id = match target {
        TaskTarget::New {
            channel,
            text,
            options,
        } => {
            let settled = crate::actors::settle(&tx, &access, actor, None, text)?;
            mentions = settled.1;
            let (root, created) =
                Board::post_tx_with_outcome(&tx, channel, actor, &settled.0, options)?;
            if !created {
                let mut t = root.task.context(
                    Conflict.error("duplicate_id", "key belongs to a thread without a task"),
                )?;
                if let Some(request) = &claim {
                    let target = fingerprint(("created", t.id))?;
                    if let Some(recovered) =
                        replay_claim(&tx, actor, request.token, Some(&target), at)?
                    {
                        t = recovered;
                    }
                }
                let t = creation_view(&tx, &access, actor, t)?;
                tx.commit()?;
                return Ok((t, false));
            }
            root.id
        }
        TaskTarget::Existing(id) => {
            if let Some(request) = &claim
                && let Some(t) = replay_claim(
                    &tx,
                    actor,
                    request.token,
                    Some(&fingerprint(("created", id))?),
                    at,
                )?
            {
                let t = visible(&access, &tx, t)?;
                tx.commit()?;
                return Ok((t, false));
            }
            require_adoptable(&tx, id)?;
            id
        }
    };
    let mut t = create_from_root(&tx, id, actor, needs, assignee)?;
    if let Some(request) = claim {
        t = acquire(&tx, actor, &t, &request, &fingerprint(("created", id))?, at)?;
    }
    remember_creation(&tx, &access, actor, &t)?;
    let mut t = creation_view(&tx, &access, actor, t)?;
    t.mentions = mentions;
    tx.commit()?;
    Ok((t, true))
}

fn require_adoptable(tx: &Transaction<'_>, id: i64) -> Result<()> {
    let root = Board::get_on(tx, id)?;
    ensure!(
        root.re.is_none() && !root.deleted,
        Invalid.error("invalid_thread", "task requires a live thread root")
    );
    ensure!(
        root.task.is_none(),
        Conflict.error("already_a_task", "thread is already a task")
    );
    Ok(())
}

pub(crate) fn create_from_root(
    tx: &Transaction<'_>,
    id: i64,
    actor: &str,
    needs: &[i64],
    assignee: Option<&str>,
) -> Result<Task> {
    tx.execute(
        "INSERT INTO tasks(id,owner,assignee,state) VALUES(?,?,?,'ready')",
        params![id, actor, assignee],
    )?;
    replace_needs(tx, id, needs)?;
    set_state(tx, id, initial_readiness(needs), None)?;
    publish_transition(tx, id, actor, "created")
}

struct BatchReceipt {
    actor: String,
    fingerprint: Vec<u8>,
    ids: String,
}

fn batch_receipt(conn: &Connection, key: &str) -> Result<Option<BatchReceipt>> {
    crate::core::validate_client_key(key)?;
    Ok(conn
        .query_row(
            "SELECT actor,fingerprint,ids FROM client_batches WHERE key=?",
            [key],
            |r| {
                Ok(BatchReceipt {
                    actor: r.get(0)?,
                    fingerprint: r.get(1)?,
                    ids: r.get(2)?,
                })
            },
        )
        .optional()?)
}

fn batch_tasks(conn: &Connection, receipt: &BatchReceipt) -> Result<Vec<Task>> {
    serde_json::from_str::<Vec<i64>>(&receipt.ids)?
        .into_iter()
        .map(|id| task(conn, id))
        .collect()
}

/// Return the current tasks in original batch order, without exposing other writers' batches.
pub fn lookup_plan(board: &Board, actor: &str, key: &str) -> Result<Vec<Task>> {
    crate::validate_actor(actor)?;
    let tx = board.conn.unchecked_transaction()?;
    let a = board.access_on(&tx)?;
    a.actor(actor)?;
    let receipt = batch_receipt(&tx, key)?
        .filter(|r| r.actor == actor)
        .context(Missing.error("batch_not_found", "no batch of yours has that key"))?;
    let tasks = batch_tasks(&tx, &receipt)?
        .into_iter()
        .map(|t| creation_view(&tx, &a, actor, t))
        .collect::<Result<Vec<_>>>()?;
    tx.commit()?;
    Ok(tasks)
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schemars", schemars(extend("oneOf" = [{"required":["text"]},{"required":["thread_id"]}])))]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct PlanItem {
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schemars", schemars(extend("not" = {"type":"null"})))]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schemars", schemars(range(min = 1), extend("not" = {"type":"null"})))]
    #[cfg_attr(feature = "utoipa", schema(minimum = 1))]
    pub thread_id: Option<i64>,
    #[serde(default)]
    pub needs: Vec<Prerequisite>,
    #[serde(rename = "for")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_ids: Vec<i64>,
    #[serde(default, alias = "noSync", skip_serializing_if = "std::ops::Not::not")]
    pub no_sync: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(untagged, deny_unknown_fields)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub enum Prerequisite {
    Key(String),
    Id(#[cfg_attr(feature = "schemars", schemars(range(min = 1)))] i64),
}

pub const MAX_PLAN_ITEMS: usize = 100;

/// Publish a bounded plan as one transaction.
pub fn create_plan(
    board: &Board,
    actor: &str,
    channel: &str,
    items: &[PlanItem],
    key: Option<&str>,
) -> Result<Vec<Task>> {
    Ok(create_plan_with_outcome(board, actor, channel, items, key)?.0)
}

pub fn create_plan_with_outcome(
    board: &Board,
    actor: &str,
    channel: &str,
    items: &[PlanItem],
    key: Option<&str>,
) -> Result<(Vec<Task>, bool)> {
    create_plan_for_run(board, actor, channel, items, key, None)
}

/// Publish ordinary tasks and optionally associate their threads with a swarm.
pub fn create_plan_for_run(
    board: &Board,
    actor: &str,
    channel: &str,
    items: &[PlanItem],
    key: Option<&str>,
    run: Option<&str>,
) -> Result<(Vec<Task>, bool)> {
    crate::validate_actor(actor)?;
    crate::validate_channel(channel)?;
    ensure!(
        (1..=MAX_PLAN_ITEMS).contains(&items.len()),
        Invalid.error("invalid_plan_size", "a plan must contain 1 to 100 tasks")
    );
    plans::create(board, actor, channel, items, key, run)
}

mod plans;

pub fn list_tasks(board: &Board, channel: Option<&str>, state: Option<&str>) -> Result<Vec<Task>> {
    Ok(tasks_page(board, channel, state, 0, usize::MAX)?.1)
}

const OPEN_STATES: &str = "t.state NOT IN ('completed','failed','canceled')";

/// The sidebar count does not load rows or claim tokens; an expiring lease is still open.
pub fn open_task_count(board: &Board) -> Result<i64> {
    let tx = board.conn.unchecked_transaction()?;
    let access = board.access_on(&tx)?;
    let (visible, bind) = crate::read::visible_predicate(&tx, &access, &crate::Filter::default())?;
    let sql = format!(
        "SELECT count(*) FROM tasks t JOIN messages m ON m.id=t.id WHERE {visible} AND {OPEN_STATES}"
    );
    let n = tx.query_row(&sql, rusqlite::params_from_iter(bind), |r| r.get(0))?;
    tx.commit()?;
    Ok(n)
}

/// Oldest task roots first. `after` is exclusive; the first return value is the snapshot's event head.
pub fn tasks_page(
    board: &Board,
    channel: Option<&str>,
    state: Option<&str>,
    after: i64,
    limit: usize,
) -> Result<(i64, Vec<Task>)> {
    filtered_tasks_page(
        board,
        &TaskFilter {
            channel: channel.map(str::to_owned),
            state: state.map(str::to_owned),
            ..Default::default()
        },
        after,
        limit,
    )
}

#[derive(Debug, Default)]
pub struct TaskFilter {
    pub latest: bool,
    pub channel: Option<String>,
    pub state: Option<String>,
    pub needs: Option<i64>,
    pub by: Option<String>,
    pub assignee: Option<String>,
    pub owner: Option<String>,
}

pub fn filtered_tasks_page(
    board: &Board,
    filter: &TaskFilter,
    after: i64,
    limit: usize,
) -> Result<(i64, Vec<Task>)> {
    filtered_tasks_page_as(board, None, filter, after, limit)
}

/// Supply the active actor for viewer-relative selectors when reading a trusted local board.
pub fn filtered_tasks_page_as(
    board: &Board,
    actor: Option<&str>,
    filter: &TaskFilter,
    after: i64,
    limit: usize,
) -> Result<(i64, Vec<Task>)> {
    ensure!(after >= 0 && limit > 0, "invalid task page");
    ensure!(
        !filter.latest || after == 0,
        "latest and after cannot be combined"
    );
    ensure!(
        filter.needs.is_none_or(|id| id > 0),
        "needs must be a positive task id"
    );
    if let Some(channel) = filter.channel.as_deref() {
        crate::validate_channel(channel.strip_suffix('/').unwrap_or(channel))?;
    }
    let state = filter.state.as_deref();
    ensure!(
        state.is_none_or(|s| matches!(
            s,
            "all"
                | "open"
                | "waiting"
                | "ready"
                | "claimed"
                | "completed"
                | "failed"
                | "canceled"
                | "blocked"
        )),
        "invalid task state"
    );
    let tx = write(&board.conn)?;
    let access = board.access_on(&tx)?;
    if let Some(id) = filter.needs {
        access.message(&tx, id, Action::Read)?;
    }
    expire(&tx, now(), &access)?;
    let mut out = Vec::new();
    let cursor = crate::read::cursor_on(&tx)?;
    let mut query = crate::Filter {
        channel: filter.channel.clone(),
        by: filter.by.clone(),
        after: Some(after),
        ..Default::default()
    };
    if let Some(actor) = actor.or(access.bound_actor()) {
        access.actor(actor)?;
        query.bind_actor(actor);
    }
    let (predicate, mut bind) = crate::read::visible_predicate(&tx, &access, &query)?;
    let mut sql = format!(
        "SELECT t.id, m.created_at, (SELECT json_group_array(a.name ORDER BY f.position)
         FROM message_files f JOIN file_assets a ON a.id=f.asset WHERE f.message_id=m.id AND NOT m.deleted)
         FROM tasks t JOIN messages m ON m.id=t.id WHERE {predicate}"
    );
    match state {
        None | Some("open") => {
            sql.push_str(" AND ");
            sql.push_str(OPEN_STATES);
        }
        Some("all") => (),
        Some(state) => {
            sql.push_str(" AND t.state=?");
            bind.push(state.to_owned().into());
        }
    }
    if let Some(need) = filter.needs {
        sql.push_str(" AND EXISTS(SELECT 1 FROM task_needs n WHERE n.need=? AND n.task=t.id)");
        bind.push(need.into());
    }
    if let Some(who) = &filter.assignee {
        sql.push_str(" AND t.assignee=?");
        bind.push(who.clone().into());
    }
    if let Some(owner) = &filter.owner {
        sql.push_str(" AND (t.owner=? OR instr(t.owner,?)=1)");
        bind.push(owner.clone().into());
        bind.push(format!("{owner}/").into());
    }
    sql.push_str(if filter.latest{" ORDER BY coalesce((SELECT max(id) FROM messages r WHERE r.re=t.id AND r.deleted=0),t.id) DESC LIMIT ?"}else{" ORDER BY t.id LIMIT ?"});
    bind.push(i64::try_from(limit).unwrap_or(i64::MAX).into());
    let mut page = tx.prepare(&sql)?;
    let mut rows = page.query(rusqlite::params_from_iter(bind))?;
    while let Some(row) = rows.next()? {
        let id = row.get(0)?;
        let mut t = visible(&access, &tx, task(&tx, id)?)?;
        t.created_at = Some(row.get(1)?);
        t.attachment_names = serde_json::from_str(row.get_ref(2)?.as_str()?)?;
        t.replies = Some(tx.query_row(
            "SELECT count(*) FROM messages WHERE re=? AND deleted=0",
            [id],
            |r| r.get(0),
        )?);
        t.last=tx.query_row("SELECT id,actor,text FROM messages WHERE re=? AND deleted=0 ORDER BY id DESC LIMIT 1",[id],|r|Ok(TaskReply{id:r.get(0)?,actor:r.get(1)?,text:r.get(2)?})).optional()?;
        out.push(t);
    }
    drop(rows);
    drop(page);
    if filter.latest {
        out.reverse()
    }
    tx.commit()?;
    Ok((cursor, out))
}

/// Acquire a lease on a specific task.
pub fn claim(board: &Board, actor: &str, id: i64, lease: i64) -> Result<Task> {
    let token = uuid::Uuid::new_v4().to_string();
    Ok(claim_in_assignment(
        board,
        actor,
        id,
        ClaimRequest {
            lease_seconds: lease,
            token: &token,
        },
        None,
    )?
    .0)
}

fn validate_claim(request: &ClaimRequest<'_>) -> Result<()> {
    ensure!(
        (30..=3600).contains(&request.lease_seconds),
        "lease must be 30 seconds to one hour"
    );
    validate_claim_token(request.token)
}
pub fn validate_claim_token(token: &str) -> Result<()> {
    ensure!(
        (16..=200).contains(&token.len())
            && token.bytes().all(|b| b.is_ascii_graphic() && b != b'"'),
        "claim token must have 16..200 printable non-space ASCII bytes without double quotes"
    );
    Ok(())
}

pub fn claim_namespace(board: &Board) -> Result<String> {
    Ok(board
        .conn
        .query_row("SELECT value FROM claim_namespace WHERE id=1", [], |r| {
            r.get(0)
        })?)
}

fn replay_claim(
    tx: &Transaction<'_>,
    actor: &str,
    token: &str,
    target: Option<&[u8]>,
    at: i64,
) -> Result<Option<Task>> {
    let saved: Option<(String, i64, i64, Vec<u8>)> = tx
        .query_row(
            "SELECT actor,task,epoch,target FROM task_receipts WHERE digest=?",
            [Sha256::digest(token.as_bytes()).as_slice()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((owner, id, epoch, stored_target)) = saved else {
        return Ok(None);
    };
    ensure!(
        owner == actor && target.is_none_or(|wanted| wanted == stored_target),
        Conflict.error(
            "claim_conflict",
            "claim token is already bound to another request"
        )
    );
    let mut t = task(tx, id)?;
    ensure!(
        t.state == "claimed"
            && t.epoch == epoch
            && t.holder.as_deref() == Some(actor)
            && t.lease_until.is_some_and(|u| u > at),
        Conflict.error("claim_lost", "claim is no longer live")
    );
    t.claim_token = Some(token.to_owned());
    Ok(Some(t))
}

/// Recover exactly the live lease minted with this proof, without claiming a second task.
pub fn recover_claim(board: &Board, actor: &str, token: &str) -> Result<Task> {
    let tx = board.conn.unchecked_transaction()?;
    let a = board.access_on(&tx)?;
    a.actor(actor)?;
    let found = replay_claim(&tx, actor, token, None, now())?
        .context(Missing.error("claim_not_found", "no claim with that proof"))?;
    a.require(Action::Task, &found.channel)?;
    let found = visible(&a, &tx, found)?;
    tx.commit()?;
    Ok(found)
}

/// Fence both the worker's run and the task claim in the same transaction.
pub fn claim_in_assignment(
    board: &Board,
    actor: &str,
    id: i64,
    request: ClaimRequest<'_>,
    worker: Option<&WorkerBinding>,
) -> Result<(Task, bool)> {
    crate::validate_actor(actor)?;
    validate_claim(&request)?;
    let tx = write(&board.conn)?;
    if let Some(worker) = worker {
        board.require_active_worker(&tx, actor, worker)?;
    }
    let access = board.access_on(&tx)?;
    access.actor(actor)?;
    access.message(&tx, id, Action::Task)?;
    let at = now();
    let proof_target = fingerprint(("id", id))?;
    let (t, created) = match replay_claim(&tx, actor, request.token, Some(&proof_target), at)? {
        Some(t) => (t, false),
        None => {
            expire_one(&tx, id, at)?;
            let t = task(&tx, id)?;
            (acquire(&tx, actor, &t, &request, &proof_target, at)?, true)
        }
    };
    let t = visible(&access, &tx, t)?;
    tx.commit()?;
    Ok((t, created))
}

fn acquire(
    tx: &Transaction<'_>,
    actor: &str,
    t: &Task,
    request: &ClaimRequest<'_>,
    target: &[u8],
    at: i64,
) -> Result<Task> {
    let id = t.id;
    ensure!(
        t.state == "ready",
        Conflict.error(
            match t.state.as_str() {
                "claimed" => "task_claimed",
                "waiting" => "task_waiting",
                "blocked" => "task_blocked",
                _ => "task_finished",
            },
            format!("task is {}, not ready", t.state)
        )
    );
    ensure!(
        t.assignee
            .as_deref()
            .is_none_or(|who| who == actor && person(actor) == person(&t.owner)),
        Forbidden.error("not_owned", "task assigned to another actor")
    );
    tx.execute(
        "UPDATE tasks SET holder=?,lease_until=?,state='claimed',epoch=epoch+1 WHERE id=?",
        params![actor, at + request.lease_seconds, id],
    )?;
    let token = request.token;
    let digest = Sha256::digest(token.as_bytes());
    ensure!(tx.execute("INSERT INTO task_receipts(digest,actor,task,epoch,target) SELECT ?,?,id,epoch,? FROM tasks WHERE id=? ON CONFLICT(digest) DO NOTHING",
        params![digest.as_slice(),actor,target,id])?==1,Conflict.error("claim_conflict","claim token is already bound to another request"));
    tx.execute(
        "INSERT OR REPLACE INTO task_claim_keys(task,digest) VALUES(?,?)",
        params![id, digest.as_slice()],
    )?;
    let mut t = publish(tx, id, actor, "claimed")?;
    t.claim_token = Some(token.to_owned());
    Ok(t)
}

pub enum TaskAction<'a> {
    Renew(i64),
    Release,
    Complete(Option<&'a str>),
    Fail(&'a str),
    Block(&'a str),
    BlockWithKind(&'a str, &'a str),
    Unblock,
    Resume,
    Cancel(&'a str),
    Reopen,
    Assign(Option<&'a str>),
    Needs(&'a [i64]),
}

pub fn change_task(
    board: &Board,
    id: i64,
    actor: &str,
    epoch: Option<i64>,
    token: Option<&str>,
    action: TaskAction<'_>,
) -> Result<Task> {
    use TaskAction::*;
    crate::validate_actor(actor)?;
    let tx = write(&board.conn)?;
    let at = now();
    let access = board.access_on(&tx)?;
    access.actor(actor)?;
    let channel = access.message(&tx, id, Action::Read)?;
    expire_one(&tx, id, at)?;
    let t = task(&tx, id)?;
    let live = t.state == "claimed"
        && t.holder.as_deref() == Some(actor)
        && Some(t.epoch) == epoch
        && match token {
            Some(token) => tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM task_claim_keys WHERE task=? AND digest=?)",
                params![id, Sha256::digest(token.as_bytes()).as_slice()],
                |r| r.get::<_, bool>(0),
            )?,
            None => false,
        };
    if matches!(action, Renew(_) | Release | Complete(_) | Fail(_))
        || matches!(action, Block(_) | BlockWithKind(_, _))
            && (t.state == "claimed" || epoch.is_some() || token.is_some())
    {
        access.require(Action::Task, &channel)?;
        ensure!(
            live,
            Conflict.error("claim_lost","claim lost or missing: a live claim by this profile, its epoch and token are required")
        );
    } else {
        ensure!(
            crate::core::owned_by(&t.by, actor)
                && (access.allows(Action::Post, &channel) || access.allows(Action::Task, &channel))
                || (!access.is_trusted() && access.allows(Action::Manage, &channel)),
            Forbidden.error("not_owned", format!("task is owned by {}", t.owner))
        );
    }
    let idle = matches!(t.state.as_str(), "waiting" | "ready" | "blocked");
    match &action {
        Block(reason) | BlockWithKind(reason, _) | Fail(reason) | Cancel(reason) => ensure!(
            !reason.trim().is_empty() && reason.chars().count() <= 1000,
            "task reason must be 1..1000 characters"
        ),
        Complete(Some(result)) => ensure!(
            result.chars().count() <= 1000,
            "task result exceeds 1000 characters"
        ),
        _ => (),
    }
    let name = match action {
        Renew(lease) => {
            ensure!(
                (30..=3600).contains(&lease),
                "lease must be 30 seconds to one hour"
            );
            tx.execute(
                "UPDATE tasks SET lease_until=? WHERE id=?",
                [at + lease, id],
            )?;
            "renewed"
        }
        Release => {
            set_state(&tx, id, ready_state(&tx, id)?, None)?;
            "released"
        }
        Complete(result) => {
            set_state(&tx, id, "completed", result)?;
            "completed"
        }
        Fail(reason) => {
            set_state(&tx, id, "failed", Some(reason))?;
            "failed"
        }
        Block(reason) | BlockWithKind(reason, _) => {
            ensure!(
                matches!(t.state.as_str(), "ready" | "waiting") || live,
                Conflict.error("task_state", "cannot block a blocked or terminal task")
            );
            set_state(&tx, id, "blocked", Some(reason))?;
            if let BlockWithKind(_, kind) = action {
                ensure!(
                    !kind.is_empty()
                        && kind.len() <= 64
                        && kind != "edited"
                        && kind.bytes().all(|c| c.is_ascii_lowercase()
                            || c.is_ascii_digit()
                            || matches!(c, b'.' | b'_' | b'-')),
                    "hold kind must have 1..64 bytes; edited is reserved"
                );
                tx.execute("INSERT INTO task_holds VALUES(?,?)", params![id, kind])?;
            }
            "blocked"
        }
        Unblock | Reopen | Resume => {
            let transition = match (action, t.state.as_str()) {
                (Unblock | Resume, "blocked") => "unblocked",
                (Reopen | Resume, "failed") => "reopened",
                (Unblock | Reopen, "ready" | "waiting" | "claimed")
                | (Resume, "ready" | "waiting")
                | (Reopen, "blocked") => {
                    let t = visible(&access, &tx, t)?;
                    tx.commit()?;
                    return Ok(t);
                }
                (Unblock, _) => anyhow::bail!(Conflict.error("task_state", "task is not blocked")),
                _ => {
                    anyhow::bail!(Conflict.error("task_state", "only failed tasks can be reopened"))
                }
            };
            ensure!(!Board::get_on(&tx, id)?.deleted, "root was deleted");
            set_state(&tx, id, initial_readiness(&t.needs), None)?;
            transition
        }
        Cancel(reason) => {
            ensure!(
                idle || matches!(t.state.as_str(), "claimed" | "failed"),
                Conflict.error("task_state", "task has ended")
            );
            set_state(&tx, id, "canceled", Some(reason))?;
            "canceled"
        }
        Assign(who) => {
            ensure!(
                idle || t.state == "claimed",
                Conflict.error("task_state", "cannot reassign a terminal task")
            );
            if let Some(who) = who {
                validate_assignee(&t.owner, who)?;
            }
            if who == t.assignee.as_deref() {
                let t = visible(&access, &tx, t)?;
                tx.commit()?;
                return Ok(t);
            }
            tx.execute("UPDATE tasks SET assignee=? WHERE id=?", params![who, id])?;
            "assigned"
        }
        Needs(needs) => {
            let distinct = distinct_needs(needs)?;
            let needs = distinct.as_slice();
            visible_needs(&access, &tx, needs)?;
            ensure!(
                idle,
                Conflict.error(
                    "task_state",
                    "cannot change prerequisites of a held or terminal task"
                )
            );
            validate_needs(&tx, id, needs)?;
            if needs.len() == t.needs.len() && needs.iter().all(|n| t.needs.contains(n)) {
                let t = visible(&access, &tx, t)?;
                tx.commit()?;
                return Ok(t);
            }
            check_added(&tx, &access, &t, needs)?;
            replace_needs(&tx, id, needs)?;
            if t.state != "blocked" {
                set_state(&tx, id, initial_readiness(needs), None)?;
            }
            "needs_changed"
        }
    };
    let mut t = publish_transition(&tx, id, actor, name)?;
    if t.state == "completed" {
        for dep in ids(
            &tx,
            "SELECT task FROM task_needs JOIN tasks ON tasks.id=task WHERE need=? AND state='waiting' ORDER BY task",
            [id],
        )? {
            if ready_state(&tx, dep)? == "ready" {
                set_state(&tx, dep, "ready", None)?;
                publish(&tx, dep, actor, "ready")?;
            }
        }
    }
    if name == "renewed" {
        t.claim_token = token.map(str::to_owned);
    }
    let t = visible(&access, &tx, t)?;
    tx.commit()?;
    Ok(t)
}

pub(crate) fn on_root_delete(tx: &Transaction<'_>, id: i64, actor: &str) -> Result<()> {
    if tx.execute(
        "UPDATE tasks SET state='canceled',holder=NULL,lease_until=NULL,result='root deleted'
        WHERE id=? AND state NOT IN ('canceled','completed')",
        [id],
    )? > 0
    {
        tx.execute("DELETE FROM task_claim_keys WHERE task=?", [id])?;
        publish(tx, id, actor, "canceled")?;
    }
    Ok(())
}
pub(crate) fn on_root_edit(tx: &Transaction<'_>, id: i64, actor: &str) -> Result<()> {
    if let Some(t) = task_optional(tx, id)?
        && matches!(t.state.as_str(), "ready" | "waiting" | "claimed")
        && !crate::core::owned_by(&t.by, actor)
    {
        set_state(tx, id, "blocked", Some("root edited by another actor"))?;
        tx.execute("INSERT INTO task_holds VALUES(?,'edited')", [id])?;
        publish(tx, id, actor, "blocked")?;
    }
    Ok(())
}
