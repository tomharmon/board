//! Local coordination records. The calling harness, never Board, launches agents.
mod feed;
pub use feed::SwarmFilter;
pub(crate) use feed::read_mention_routing;
mod assignment;
mod coordination;
pub use assignment::{Assignment, AssignmentFile};
mod materialize;
mod observe;
pub use observe::{RunAction, RunChange, RunThread, SwarmObservation, TaskExplanation};
mod wait;
use crate::{
    Board, Message, PostOptions,
    access::{Access, Action},
    errors::Kind::{Conflict, Forbidden, Invalid, Missing},
};
use anyhow::{Context, Result, ensure};
pub use coordination::*;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
pub use wait::{SwarmInboxWait, SwarmWaitOutcome};

pub(crate) fn init(c: &Connection) -> Result<()> {
    let legacy: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='swarm_workers') AND NOT EXISTS(SELECT 1 FROM pragma_table_info('swarm_workers') WHERE name='checked_in_at')", [], |r| r.get(0))?;
    if legacy {
        c.execute_batch("ALTER TABLE swarm_workers RENAME TO swarm_workers_legacy;")?;
    }
    c.execute_batch(include_str!("swarm/schema.sql"))?;
    if legacy {
        // Old working rows did not prove a worker ever started. Do not invent check-ins.
        c.execute_batch("INSERT INTO swarm_workers(id,run_id,name,actor,role_slug,pinned_role_version,pinned_system_prompt,pinned_skills_json,brief,workspace_path,branch_name,status,waiter_pid,last_read_cursor,created_at,updated_at,checked_in_at,last_activity_at)
            SELECT id,run_id,name,actor,role_slug,pinned_role_version,pinned_system_prompt,pinned_skills_json,brief,workspace_path,branch_name,CASE WHEN status='working' THEN 'registered' ELSE status END,waiter_pid,last_read_cursor,created_at,updated_at,CASE WHEN status='waiting' THEN updated_at END,CASE WHEN status='waiting' THEN updated_at END FROM swarm_workers_legacy;
            DROP TABLE swarm_workers_legacy;
            CREATE INDEX IF NOT EXISTS swarm_workers_by_run ON swarm_workers(run_id,id);")?;
    }
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SwarmStart {
    pub goal: String,
    pub slug: Option<String>,
    pub repo_root: Option<String>,
    pub max_notifications: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RunPolicy {
    pub max_active: i64,
    pub retry_budget: i64,
}
impl Default for RunPolicy {
    fn default() -> Self {
        Self {
            max_active: 8,
            retry_budget: 0,
        }
    }
}
impl RunPolicy {
    fn validate(self) -> Result<()> {
        ensure!(
            (1..=128).contains(&self.max_active) && (0..=1000).contains(&self.retry_budget),
            Invalid.error(
                "invalid_policy",
                "max_active must be 1..128 and retry_budget 0..1000"
            )
        );
        Ok(())
    }
    fn read(c: &Connection, id: i64) -> Result<Self> {
        Ok(c.query_row(
            "SELECT max_active,retry_budget FROM swarm_policy WHERE run_id=?",
            [id],
            |r| {
                Ok(Self {
                    max_active: r.get(0)?,
                    retry_budget: r.get(1)?,
                })
            },
        )
        .optional()?
        .unwrap_or_default())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SwarmAddWorker {
    pub name: String,
    pub role_slug: Option<String>,
    pub brief: String,
    pub workspace_path: Option<String>,
    pub branch_name: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct SwarmSummary {
    pub id: i64,
    pub slug: String,
    pub goal: String,
    pub channel: String,
    pub coordinator_actor: String,
    pub status: String,
    pub worker_count: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmRun {
    pub id: i64,
    pub slug: String,
    pub goal: String,
    pub channel: String,
    pub repo_root: Option<String>,
    pub coordinator_actor: String,
    /// The current viewer may configure or stop this run, not necessarily consume its inbox.
    pub can_control: bool,
    pub status: String,
    pub max_notifications: i64,
    pub notifications_sent: i64,
    pub created_at: String,
    pub ended_at: Option<String>,
    pub workers: Vec<SwarmWorker>,
    pub activity_count: i64,
    #[serde(skip)]
    root_id: i64,
}
impl SwarmRun {
    fn operator(&self, actor: &str) -> bool {
        crate::core::owned_by(&self.coordinator_actor, actor)
    }
    fn may_control(access: &Access, channel: &str, coordinator: &str, actor: &str) -> bool {
        access.allows(Action::Post, channel)
            && (crate::core::owned_by(coordinator, actor)
                || (access.is_trusted() && !actor.contains('/')))
    }
    fn control(&self) -> Result<()> {
        ensure!(
            self.can_control,
            Forbidden.error(
                "swarm_coordinator_required",
                "only the coordinator, its owning person, or the local board owner can do that"
            )
        );
        Ok(())
    }
    fn active(&self) -> Result<()> {
        ensure!(
            self.status == "active",
            Conflict.error("swarm_stopped", "the swarm has ended")
        );
        Ok(())
    }
    fn name_for(&self, actor: &str) -> Option<&str> {
        if self.coordinator_actor == actor {
            Some("coordinator")
        } else {
            self.workers
                .iter()
                .find(|w| w.actor == actor)
                .map(|w| w.name.as_str())
        }
    }
    fn can_see_dm(&self, actor: &str, a: &Access, all: bool) -> bool {
        self.operator(actor) || (all && a.is_trusted() && !actor.contains('/'))
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmWorker {
    pub id: i64,
    pub run_id: i64,
    pub name: String,
    pub actor: String,
    pub role_slug: Option<String>,
    pub pinned_role_version: Option<i64>,
    pub brief: String,
    pub workspace_path: Option<String>,
    pub branch_name: Option<String>,
    pub status: String,
    pub waiter_pid: Option<u32>,
    pub last_read_cursor: i64,
    pub created_at: String,
    pub updated_at: String,
    pub checked_in_at: Option<String>,
    pub last_activity_at: Option<String>,
    pub launch_error: Option<String>,
    /// Self-reported details; never credentials, commands, or proof of liveness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_ref: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnedWorker {
    pub worker: SwarmWorker,
    pub assembled_prompt: String,
    pub skills_dir: PathBuf,
    pub env_vars: BTreeMap<String, String>,
    /// Arguments before the subcommand. An in-memory board cannot be shared between processes.
    pub cli_args: Option<Vec<String>>,
}
#[derive(Debug, Clone, Serialize)]
pub struct WorkerBinding {
    pub run: String,
    pub run_id: i64,
    pub name: String,
    pub actor: String,
}
impl WorkerBinding {
    pub fn selector(s: &str) -> Result<(&str, &str)> {
        s.split_once('/')
            .filter(|(r, n)| valid_slug(r, 64) && valid_slug(n, 24))
            .context(Invalid.error("invalid_worker", "expected an assigned RUN/NAME"))
    }
    pub fn select_run(&self, requested: Option<&str>) -> Result<String> {
        ensure!(
            requested.is_none_or(|r| r == self.run
                || r.strip_prefix('#').and_then(|i| i.parse::<i64>().ok()) == Some(self.run_id)),
            Forbidden.error(
                "worker_run_mismatch",
                "the worker assignment is for a different run"
            )
        );
        Ok(self.run.clone())
    }
    pub fn coordinator_only() -> anyhow::Error {
        Forbidden
            .error(
                "swarm_coordinator_required",
                "this command is not available from a worker assignment",
            )
            .into()
    }
}
#[derive(Debug, Serialize)]
pub struct RegisteredWorker {
    pub run: String,
    pub channel: String,
    pub selector: String,
    pub worker: SwarmWorker,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmPostResult {
    pub message: Message,
    pub notified: Vec<String>,
    pub suppressed: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmDm {
    pub id: i64,
    pub run_id: i64,
    pub from_actor: String,
    pub to_worker: String,
    pub text: String,
    pub event_cursor: i64,
    pub created_at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmEntry {
    pub cursor: i64,
    pub kind: String,
    pub message: Option<Message>,
    pub dm: Option<SwarmDm>,
    pub notified: Vec<String>,
    #[serde(default)]
    pub suppressed: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmFeed {
    pub entries: Vec<SwarmEntry>,
    pub cursor: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmNotification {
    pub id: i64,
    pub run_id: i64,
    pub target_worker: String,
    pub source_kind: String,
    pub from_actor: String,
    pub message_id: Option<i64>,
    pub dm_id: Option<i64>,
    pub event_cursor: i64,
    pub created_at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WaitPoll {
    Pending,
    Ready {
        cursor: i64,
        notifications: Vec<SwarmNotification>,
        messages: Vec<Message>,
        dms: Vec<SwarmDm>,
    },
    Stopped {
        cursor: i64,
    },
}

pub(crate) fn valid_slug(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_'))
}
fn location(s: Option<&str>) -> Result<()> {
    ensure!(
        s.is_none_or(|v| !v.is_empty() && v.len() <= 4096 && !v.chars().any(char::is_control)),
        Invalid.error(
            "invalid_swarm_location",
            "expected a short printable path or branch"
        )
    );
    Ok(())
}
fn rows<T>(it: impl Iterator<Item = rusqlite::Result<T>>) -> Result<Vec<T>> {
    Ok(it.collect::<rusqlite::Result<_>>()?)
}
const WORKER_SELECT: &str = "SELECT w.id,w.run_id,w.name,w.actor,w.role_slug,w.pinned_role_version,w.brief,w.workspace_path,w.branch_name,w.status,w.waiter_pid,w.last_read_cursor,w.created_at,w.updated_at,w.checked_in_at,w.last_activity_at,w.launch_error,p.model,p.workspace_mode,p.session_ref FROM swarm_workers w LEFT JOIN swarm_worker_runtime p ON p.worker_id=w.id";
fn worker(r: &rusqlite::Row) -> rusqlite::Result<SwarmWorker> {
    Ok(SwarmWorker {
        id: r.get(0)?,
        run_id: r.get(1)?,
        name: r.get(2)?,
        actor: r.get(3)?,
        role_slug: r.get(4)?,
        pinned_role_version: r.get(5)?,
        brief: r.get(6)?,
        workspace_path: r.get(7)?,
        branch_name: r.get(8)?,
        status: r.get(9)?,
        waiter_pid: r.get(10)?,
        last_read_cursor: r.get(11)?,
        created_at: r.get(12)?,
        updated_at: r.get(13)?,
        checked_in_at: r.get(14)?,
        last_activity_at: r.get(15)?,
        launch_error: r.get(16)?,
        model: r.get(17)?,
        workspace_mode: r.get(18)?,
        session_ref: r.get(19)?,
    })
}

impl Board {
    pub(crate) fn require_active_worker(
        &self,
        c: &Connection,
        actor: &str,
        w: &WorkerBinding,
    ) -> Result<()> {
        let (_, r) = self.swarm_record(c, actor, &w.run, Action::Read)?;
        r.active()?;
        ensure!(r.id==w.run_id && c.query_row("SELECT EXISTS(SELECT 1 FROM swarm_workers WHERE run_id=? AND actor=? AND name=? AND status NOT IN ('done','stopped'))",params![r.id,actor,w.name],|r|r.get::<_,bool>(0))?,
            Conflict.error("worker_stopped","no active assignment for this worker"));
        Ok(())
    }
    pub fn worker_binding(&self, selector: &str) -> Result<WorkerBinding> {
        let (slug, name) = WorkerBinding::selector(selector)?;
        let tx = self.conn.unchecked_transaction()?;
        // Select from the roster, not the caller's inherited chat profile. A scoped session
        // must match the actor AND be allowed to read the run before anything is returned.
        let access = self.access_on(&tx)?;
        access.require(Action::Read, &format!("swarms/{slug}"))?;
        let (actor,run_id):(String,i64)=tx.query_row(
            "SELECT w.actor,w.run_id FROM swarm_workers w JOIN swarm_runs r ON r.id=w.run_id WHERE r.slug=? AND w.name=?",
            params![slug,name], |r|Ok((r.get(0)?,r.get(1)?))).optional()?
            .context(Forbidden.error("worker_identity_mismatch","no matching worker assignment for this actor and run"))?;
        access.actor(&actor)?;
        tx.commit()?;
        Ok(WorkerBinding {
            run: slug.into(),
            run_id,
            name: name.into(),
            actor,
        })
    }
    fn swarm_record(
        &self,
        c: &Connection,
        actor: &str,
        reference: &str,
        action: Action,
    ) -> Result<(Access, SwarmRun)> {
        crate::validate_actor(actor)?;
        let a = self.access_on(c)?;
        a.actor(actor)?;
        let number = reference
            .strip_prefix('#')
            .unwrap_or(reference)
            .parse::<i64>()
            .ok();
        let rn=c.query_row("SELECT id,slug,goal,channel,repo_root,coordinator_actor,status,max_notifications,notifications_sent,created_at,ended_at,root_id FROM swarm_runs WHERE slug=?1 OR id=?2 ORDER BY (slug=?1) DESC LIMIT 1",
            params![reference,number],|r| Ok(SwarmRun{id:r.get(0)?,slug:r.get(1)?,goal:r.get(2)?,channel:r.get(3)?,repo_root:r.get(4)?,coordinator_actor:r.get(5)?,can_control:false,status:r.get(6)?,max_notifications:r.get(7)?,notifications_sent:r.get(8)?,created_at:r.get(9)?,ended_at:r.get(10)?,root_id:r.get(11)?,workers:vec![],activity_count:0})).optional()?;
        let mut run = rn
            .filter(|v| a.allows(Action::Read, &v.channel))
            .context(Missing.error("swarm_not_found", "swarm not found"))?;
        a.require(action, &run.channel)?;
        run.can_control = SwarmRun::may_control(&a, &run.channel, &run.coordinator_actor, actor);
        Ok((a, run))
    }
    fn swarm_on(
        &self,
        c: &Connection,
        actor: &str,
        reference: &str,
        action: Action,
    ) -> Result<(Access, SwarmRun)> {
        let (a, mut run) = self.swarm_record(c, actor, reference, action)?;
        run.workers = rows(
            c.prepare(&format!("{WORKER_SELECT} WHERE w.run_id=? ORDER BY w.id"))?
                .query_map([run.id], worker)?,
        )?;
        Ok((a, run))
    }
    fn swarm_overview(&self, c: &Connection, actor: &str, reference: &str) -> Result<SwarmRun> {
        let (a, mut run) = self.swarm_on(c, actor, reference, Action::Read)?;
        let full = run.can_see_dm(actor, &a, true);
        run.activity_count=c.query_row("SELECT (SELECT count(*) FROM messages WHERE channel=?1 AND id>=?2)+(SELECT count(*) FROM swarm_dms WHERE run_id=?3 AND (?4 OR from_actor=?5 OR to_worker=?6))",
            params![run.channel,run.root_id,run.id,full,actor,run.name_for(actor)],|r|r.get(0))?;
        Ok(run)
    }
    pub fn swarm_status(&self, actor: &str, run: &str) -> Result<SwarmRun> {
        let tx = self.conn.unchecked_transaction()?;
        let r = self.swarm_overview(&tx, actor, run)?;
        tx.commit()?;
        Ok(r)
    }
    /// Run picker: count members, without fetching their briefs or inspecting private feeds.
    pub fn swarm_summaries(
        &self,
        actor: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<SwarmSummary>> {
        ensure!(
            (1..=500).contains(&limit),
            Invalid.error("invalid_limit", "limit must be between 1 and 500")
        );
        let tx = self.conn.unchecked_transaction()?;
        let a = self.access_on(&tx)?;
        a.actor(actor)?;
        let channels = a
            .channels(&tx, Action::Read)?
            .map(|c| serde_json::to_string(&c))
            .transpose()?;
        let result = rows(
            tx.prepare(
                "SELECT r.id,r.slug,r.goal,r.channel,r.coordinator_actor,r.status,
                    (SELECT count(*) FROM swarm_workers w WHERE w.run_id=r.id)
             FROM swarm_runs r WHERE r.id<coalesce(?1,9223372036854775807)
                AND (?2 IS NULL OR r.channel IN (SELECT value FROM json_each(?2)))
             ORDER BY r.id DESC LIMIT ?3",
            )?
            .query_map(params![before, channels, limit as i64], |s| {
                Ok(SwarmSummary {
                    id: s.get(0)?,
                    slug: s.get(1)?,
                    goal: s.get(2)?,
                    channel: s.get(3)?,
                    coordinator_actor: s.get(4)?,
                    status: s.get(5)?,
                    worker_count: s.get(6)?,
                })
            })?,
        )?;
        tx.commit()?;
        Ok(result)
    }
    pub fn swarm_list(
        &self,
        actor: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<SwarmRun>> {
        ensure!(
            (1..=500).contains(&limit),
            Invalid.error("invalid_limit", "limit must be between 1 and 500")
        );
        let tx = self.conn.unchecked_transaction()?;
        let a = self.access_on(&tx)?;
        a.actor(actor)?;
        let channels = a
            .channels(&tx, Action::Read)?
            .map(|c| serde_json::to_string(&c))
            .transpose()?;
        let ids=rows(tx.prepare("SELECT id FROM swarm_runs WHERE id<coalesce(?1,9223372036854775807) AND (?2 IS NULL OR channel IN (SELECT value FROM json_each(?2))) ORDER BY id DESC LIMIT ?3")?.query_map(params![before,channels,limit as i64],|r|r.get::<_,i64>(0))?)?;
        let runs = ids
            .iter()
            .map(|id| self.swarm_overview(&tx, actor, &format!("#{id}")))
            .collect::<Result<_>>()?;
        tx.commit()?;
        Ok(runs)
    }
    pub fn swarm_active_count(&self, actor: &str) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        let a = self.access_on(&tx)?;
        a.actor(actor)?;
        let channels = a
            .channels(&tx, Action::Read)?
            .map(|c| serde_json::to_string(&c))
            .transpose()?;
        let count=tx.query_row("SELECT count(*) FROM swarm_runs WHERE status='active' AND (?1 IS NULL OR channel IN (SELECT value FROM json_each(?1)))",[channels],|r|r.get(0))?;
        tx.commit()?;
        Ok(count)
    }
    pub fn swarm_start(&self, actor: &str, args: &SwarmStart) -> Result<SwarmRun> {
        self.swarm_start_with_policy(actor, args, RunPolicy::default())
    }
    pub fn swarm_start_with_policy(
        &self,
        actor: &str,
        args: &SwarmStart,
        policy: RunPolicy,
    ) -> Result<SwarmRun> {
        policy.validate()?;
        let slug = args
            .slug
            .clone()
            .unwrap_or_else(|| format!("run-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]));
        ensure!(
            valid_slug(&slug, 64),
            Invalid.error("invalid_swarm_slug", "invalid run slug")
        );
        location(args.repo_root.as_deref())?;
        let channel = format!("swarms/{slug}");
        let max = args.max_notifications.unwrap_or(200);
        notification_limit(max)?;
        self.write(|tx|{
            let a=self.access_on(tx)?;a.actor(actor)?;a.require(Action::Read,&channel)?;a.require(Action::Post,&channel)?;
            ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM channels WHERE name=?)",[&channel],|r|r.get::<_,bool>(0))?,Conflict.error("swarm_exists","run channel already exists"));
            let (goal,_)=crate::actors::settle(tx,&a,actor,None,&args.goal)?;
            let m=Self::post_tx(tx,&channel,actor,&goal,&PostOptions{no_sync:true,..Default::default()})?;
            tx.execute("INSERT INTO swarm_runs(slug,goal,channel,root_id,repo_root,coordinator_actor,status,max_notifications,created_at) VALUES(?,?,?,?,?,?,'active',?,?)",
                params![slug,m.text,channel,m.id,args.repo_root,actor,max,m.created_at])?;
            tx.execute("INSERT INTO swarm_policy(run_id,max_active,retry_budget) SELECT id,?,? FROM swarm_runs WHERE slug=?",params![policy.max_active,policy.retry_budget,slug])?;
            self.swarm_overview(tx,actor,&slug)
        })
    }
    pub fn swarm_add_worker(
        &self,
        actor: &str,
        run: &str,
        args: &SwarmAddWorker,
    ) -> Result<SpawnedWorker> {
        let registered = self.swarm_register_worker(actor, run, args)?;
        let tx = self.conn.unchecked_transaction()?;
        let (access, r) = self.swarm_record(&tx, actor, &registered.run, Action::Post)?;
        r.control()?;
        r.active()?;
        if registered.worker.role_slug.is_some() {
            access.trusted()?;
        }
        let (prompt, skills) = Self::pinned_assignment(&tx, registered.worker.id)?;
        tx.commit()?;
        self.materialize_spawn(&r, registered.worker, &prompt, &skills)
    }
    pub fn swarm_register_worker(
        &self,
        actor: &str,
        run: &str,
        args: &SwarmAddWorker,
    ) -> Result<RegisteredWorker> {
        ensure!(
            valid_slug(&args.name, 24) && !matches!(args.name.as_str(), "all" | "coordinator"),
            Invalid.error("invalid_worker_name", "invalid or reserved worker name")
        );
        ensure!(
            !args.brief.trim().is_empty() && args.brief.len() <= 16_000,
            Invalid.error("invalid_brief", "worker brief must contain 1..16000 bytes")
        );
        location(args.workspace_path.as_deref())?;
        location(args.branch_name.as_deref())?;
        self.write(|tx|{
            let (access,r)=self.swarm_on(tx,actor,run,Action::Post)?;r.control()?;r.active()?;
            let person=r.coordinator_actor.split('/').next().unwrap_or(&r.coordinator_actor);
            let who=format!("{person}/{}",args.name);crate::validate_actor(&who)?;
            ensure!(who!=r.coordinator_actor,Conflict.error("worker_exists","the coordinator already uses this profile"));
            if args.role_slug.is_some() {
                // A scoped channel credential is not permission to export the private role library.
                access.trusted()?;
            }
            let registered = |worker| RegisteredWorker {run:r.slug.clone(),channel:r.channel.clone(),selector:format!("{}/{}",r.slug,args.name),worker};
            if let Some(w)=r.workers.iter().find(|w|w.name==args.name){
                ensure!(w.role_slug==args.role_slug && w.brief==args.brief && w.workspace_path==args.workspace_path && w.branch_name==args.branch_name, Conflict.error("worker_exists","that worker name already has a different assignment"));
                return Ok(registered(w.clone()));
            }
            ensure!(r.workers.len()<128,Invalid.error("worker_limit","a run may have up to 128 workers"));
            let (version,prompt,skills)=if let Some(slug)=&args.role_slug {
                let (v,p,s)=crate::roles::pin_on(tx,slug)?;(Some(v),p,s)
            }else{(None,String::new(),vec![])};
            let time=crate::now();let body=format!("Brief for @[{who}]:\n{}",args.brief);
            let m=Self::post_tx(tx,&r.channel,actor,&body,&PostOptions{no_sync:true,..Default::default()})?;
            tx.execute("INSERT INTO swarm_workers(run_id,name,actor,role_slug,pinned_role_version,pinned_system_prompt,pinned_skills_json,brief,workspace_path,branch_name,status,last_read_cursor,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,'registered',?,?,?)",
                params![r.id,args.name,who,args.role_slug,version,prompt,serde_json::to_string(&skills)?,args.brief,args.workspace_path,args.branch_name,m.id,time,time])?;
            let w=tx.query_row(&format!("{WORKER_SELECT} WHERE w.run_id=? AND w.name=?"),params![r.id,args.name],worker)?;
            Board::event_tx(tx,"swarm.registered",actor,Some(m.id),serde_json::json!({"run":r.slug,"worker":w.name}))?;
            Ok(registered(w))
        })
    }
    pub fn swarm_stop(&self, actor: &str, run: &str) -> Result<SwarmRun> {
        self.write(|tx|{let (_,r)=self.swarm_on(tx,actor,run,Action::Post)?;r.control()?;
            if r.status=="active" {
                let stop=Board::event_tx(tx,"swarm.stopped",actor,Some(r.root_id),serde_json::json!({"run":r.slug}))?;
                let time=crate::now();
                tx.execute("UPDATE swarm_runs SET status='stopped',ended_at=?,stop_cursor=?,coordinator_pid=NULL WHERE id=?",params![time,stop,r.id])?;
                tx.execute("UPDATE swarm_workers SET status='stopped',waiter_pid=NULL,updated_at=? WHERE run_id=? AND status!='done'",params![time,r.id])?;
            } self.swarm_overview(tx,actor,run)})
    }
    pub fn swarm_set_limit(&self, actor: &str, run: &str, max: i64) -> Result<SwarmRun> {
        notification_limit(max)?;
        self.write(|tx| {
            let (_, r) = self.swarm_on(tx, actor, run, Action::Post)?;
            r.control()?;
            r.active()?;
            if max != r.max_notifications {
                tx.execute(
                    "UPDATE swarm_runs SET max_notifications=? WHERE id=?",
                    params![max, r.id],
                )?;
                Board::event_tx(
                    tx,
                    "swarm.limit",
                    actor,
                    Some(r.root_id),
                    serde_json::json!({"run":r.slug,"limit":max}),
                )?;
            }
            self.swarm_overview(tx, actor, run)
        })
    }
}
fn notification_limit(v: i64) -> Result<()> {
    ensure!(
        (0..=100_000).contains(&v),
        Invalid.error(
            "invalid_notification_limit",
            "notification limit must be between 0 and 100000"
        )
    );
    Ok(())
}
