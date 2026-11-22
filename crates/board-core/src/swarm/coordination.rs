use super::*;
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub kind: String,
    pub value: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handoff {
    pub revision: Revision,
    pub summary: String,
    pub changes: Vec<String>,
    pub to: String,
    pub task: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationCheck {
    pub name: String,
    pub status: String,
    pub command: String,
    pub output: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verification {
    pub handoff: i64,
    pub revision: Revision,
    pub summary: String,
    pub checks: Vec<VerificationCheck>,
}

/// Source posts for the latest visible handoff of a task and its exact revision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskHandoff {
    pub id: i64,
    pub revision: Revision,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<TaskVerification>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskVerification {
    pub id: i64,
    pub status: String,
}

pub(crate) fn task_handoff_on(
    c: &Connection,
    access: &Access,
    task: i64,
    run: Option<i64>,
) -> Result<Option<TaskHandoff>> {
    // A run can hand off a task from another channel. A grant on the task
    // alone must never disclose that run's evidence or even its post IDs.
    let mut stmt = c.prepare_cached(
        "SELECT r.channel,h.message_id,json_extract(h.payload,'$.revision.kind'),
            json_extract(h.payload,'$.revision.value'),json_extract(h.payload,'$.to'),
            v.message_id,v.payload,hm.deleted,coalesce(vm.deleted,0)
         FROM (SELECT id,channel FROM swarm_runs WHERE id=?2
               UNION ALL SELECT id,channel FROM swarm_runs WHERE ?2 IS NULL) r
         JOIN swarm_evidence h ON h.message_id=(
             SELECT max(message_id) FROM swarm_evidence WHERE run_id=r.id AND kind='handoff'
             AND json_extract(payload,'$.task')=?1)
         JOIN messages hm ON hm.id=h.message_id
         LEFT JOIN swarm_evidence v ON v.message_id=(
             SELECT max(message_id) FROM swarm_evidence WHERE run_id=r.id AND kind='verification'
             AND json_extract(payload,'$.handoff')=h.message_id)
         LEFT JOIN messages vm ON vm.id=v.message_id
         ORDER BY h.message_id DESC",
    )?;
    let mut rows = stmt.query(params![task, run])?;
    while let Some(row) = rows.next()? {
        if !access.allows(Action::Read, row.get_ref(0)?.as_str()?) {
            continue;
        }
        if row.get::<_, bool>(7)? {
            return Ok(None);
        }
        let verification = match (row.get::<_, Option<i64>>(5)?, row.get::<_, bool>(8)?) {
            (Some(id), false) => Some(TaskVerification {
                id,
                status: serde_json::from_str::<Verification>(row.get_ref(6)?.as_str()?)?
                    .status()
                    .into(),
            }),
            _ => None,
        };
        return Ok(Some(TaskHandoff {
            id: row.get(1)?,
            revision: Revision {
                kind: row.get(2)?,
                value: row.get(3)?,
            },
            to: row.get(4)?,
            verification,
        }));
    }
    Ok(None)
}

impl Verification {
    fn status(&self) -> &str {
        if self.checks.iter().any(|c| c.status == "fail") {
            "fail"
        } else if self.checks.is_empty() || self.checks.iter().any(|c| c.status == "not-run") {
            "not-run"
        } else {
            "pass"
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRequest {
    pub to: String,
    pub summary: String,
    pub handoff: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionResolution {
    pub request: i64,
    pub summary: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandoffReceipt {
    pub id: i64,
    pub actor: String,
    pub handoff: Handoff,
    pub superseded: bool,
    pub status: String,
    pub verification_id: Option<i64>,
    pub verified_by: Option<String>,
    pub verification: Option<Verification>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingDecision {
    pub id: i64,
    pub actor: String,
    pub request: DecisionRequest,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SwarmReceipt {
    pub run_status: String,
    pub handoffs: Vec<HandoffReceipt>,
    pub decisions: Vec<PendingDecision>,
    pub open_tasks: Vec<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AttentionItem {
    pub kind: String,
    pub action_required: bool,
    pub summary: String,
    pub waiting: String,
    pub assigned_to: String,
    pub message_id: i64,
    pub run: Option<String>,
    pub run_id: Option<i64>,
    pub decision_id: Option<i64>,
}

pub(crate) fn coordination_schemas() -> Vec<(&'static str, &'static str, Value)> {
    let text = json!({"type":"string","minLength":1,"maxLength":4000,"pattern":"\\S"});
    let id = json!({"type":"integer","minimum":1});
    let revision = json!({"type":"object","additionalProperties":false,"required":["kind","value"],"properties":{
        "kind":{"enum":["git-commit","sha256"]},"value":{"type":"string","pattern":"^(?:[0-9a-f]{40}|[0-9a-f]{64})$"}}});
    vec![
        (
            "handoff",
            "An immutable revision handed to a named verifier.",
            json!({"type":"object","additionalProperties":false,"required":["revision","summary","changes","to"],"properties":{
            "revision":revision,"summary":text,"changes":{"type":"array","minItems":1,"maxItems":100,"items":text},"to":text,"task":id}}),
        ),
        (
            "verification",
            "Checks against one exact handoff revision; never proof of another revision.",
            json!({"type":"object","additionalProperties":false,"required":["handoff","revision","summary","checks"],"properties":{
            "handoff":id,"revision":revision,"summary":text,"checks":{"type":"array","minItems":1,"maxItems":100,"items":{"type":"object","additionalProperties":false,"required":["name","status","command"],"properties":{
                "name":text,"status":{"enum":["pass","fail","not-run"]},"command":text,"output":text}}}}}),
        ),
        (
            "decision-request",
            "An explicit request addressed to an exact actor.",
            json!({"type":"object","additionalProperties":false,"required":["to","summary"],"properties":{
            "to":text,"summary":text,"handoff":id}}),
        ),
        (
            "decision-resolution",
            "An answer by the requested actor or their owning person.",
            json!({"type":"object","additionalProperties":false,"required":["request","summary"],"properties":{
            "request":id,"summary":text}}),
        ),
    ]
}

fn record<T: for<'de> Deserialize<'de>>(
    c: &Connection,
    run: i64,
    id: i64,
    kind: &str,
) -> Result<(String, T)> {
    let (actor, data): (String, String) = c
        .query_row(
            "SELECT actor,payload FROM swarm_evidence WHERE run_id=? AND message_id=? AND kind=?",
            params![run, id, kind],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .context(Invalid.error(
            "invalid_evidence_reference",
            "reference must name a matching record in this run",
        ))?;
    Ok((actor, serde_json::from_str(&data)?))
}

fn coordination_blocks<'a>(c: &Connection, m: &'a Message) -> Result<Vec<&'a crate::Block>> {
    let mut blocks = Vec::new();
    for block in &m.blocks {
        if block.version == 1 && matches!(block.schema.as_str(), "handoff"|"verification"|"decision-request"|"decision-resolution")
            && c.query_row("SELECT coalesce((SELECT builtin FROM schema_origins WHERE name=? AND version=1),0)", [&block.schema], |r| r.get::<_,bool>(0))? {
            blocks.push(block);
        }
    }
    Ok(blocks)
}

pub(super) fn record_on(
    tx: &Transaction,
    a: &Access,
    run: Option<&SwarmRun>,
    m: &Message,
) -> Result<Option<String>> {
    let blocks = coordination_blocks(tx, m)?;
    ensure!(
        blocks.len() <= 1,
        Invalid.error(
            "ambiguous_evidence",
            "post one coordination record at a time"
        )
    );
    let Some(block) = blocks.first() else {
        return Ok(None);
    };
    let run = run.context(Invalid.error(
        "swarm_required",
        "coordination records require a readable swarm channel",
    ))?;
    if block.schema != "decision-resolution" {
        run.active()?;
    }
    let target = match block.schema.as_str() {
        "handoff" => {
            let h: Handoff = serde_json::from_value(block.data.clone())?;
            ensure!(
                run.name_for(&m.actor).is_some(),
                Forbidden.error("worker_required", "handoffs require a run participant")
            );
            ensure!(
                run.workers.iter().any(|w| w.name == h.to
                    && w.actor != m.actor
                    && !matches!(w.status.as_str(), "done" | "stopped")),
                Invalid.error(
                    "invalid_verifier",
                    "name another registered worker as verifier"
                )
            );
            validate_revision(&h.revision)?;
            if let Some(task) = h.task {
                a.message(tx, task, Action::Read)?;
                ensure!(
                    crate::automation::task_optional(tx, task)?.is_some(),
                    "handoff task is not a task"
                );
            }
            Some(h.to)
        }
        "verification" => {
            let v: Verification = serde_json::from_value(block.data.clone())?;
            let (author, h): (_, Handoff) = record(tx, run.id, v.handoff, "handoff")?;
            let verifier = run
                .workers
                .iter()
                .find(|w| w.name == h.to)
                .context("verifier no longer registered")?;
            ensure!(
                verifier.actor == m.actor,
                Forbidden.error(
                    "wrong_verifier",
                    "only the named verifier may report this handoff"
                )
            );
            ensure!(
                v.revision == h.revision,
                Conflict.error(
                    "revision_mismatch",
                    "verified revision does not match the handoff"
                )
            );
            validate_revision(&v.revision)?;
            run.name_for(&author).map(str::to_owned)
        }
        "decision-request" => {
            let d: DecisionRequest = serde_json::from_value(block.data.clone())?;
            crate::validate_actor(&d.to)?;
            ensure!(
                run.name_for(&m.actor).is_some() || run.can_control,
                Forbidden.error(
                    "worker_required",
                    "decision requests require a participant or operator"
                )
            );
            if let Some(id) = d.handoff {
                let _: (String, Handoff) = record(tx, run.id, id, "handoff")?;
            }
            run.name_for(&d.to)
                .or_else(|| run.operator(&d.to).then_some("coordinator"))
                .map(str::to_owned)
        }
        "decision-resolution" => {
            let d: DecisionResolution = serde_json::from_value(block.data.clone())?;
            let (author, request): (_, DecisionRequest) =
                record(tx, run.id, d.request, "decision-request")?;
            ensure!(
                crate::core::owned_by(&request.to, &m.actor),
                Forbidden.error(
                    "wrong_decision_actor",
                    "only the addressed actor or their person may answer"
                )
            );
            let resolved: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM swarm_evidence WHERE run_id=? AND kind='decision-resolution' AND json_extract(payload,'$.request')=?)",params![run.id,d.request],|r|r.get(0))?;
            ensure!(
                !resolved,
                Conflict.error("decision_resolved", "this request already has an answer")
            );
            run.name_for(&author).map(str::to_owned)
        }
        _ => unreachable!(),
    };
    tx.execute(
        "INSERT INTO swarm_evidence VALUES(?,?,?,?,?,?)",
        params![
            m.id,
            run.id,
            block.schema,
            m.actor,
            block.data.to_string(),
            m.created_at
        ],
    )?;
    Ok(target.filter(|t| run.name_for(&m.actor) != Some(t.as_str())))
}

fn validate_revision(r: &Revision) -> Result<()> {
    ensure!(
        r.value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            && match r.kind.as_str() {
                "git-commit" => matches!(r.value.len(), 40 | 64),
                "sha256" => r.value.len() == 64,
                _ => false,
            },
        Invalid.error(
            "invalid_revision",
            "use an immutable git commit or SHA-256 digest"
        )
    );
    Ok(())
}

fn receipt_on(c: &Connection, run: &SwarmRun) -> Result<SwarmReceipt> {
    let records = rows(c.prepare("SELECT message_id,kind,actor,payload FROM swarm_evidence WHERE run_id=? ORDER BY message_id")?.query_map([run.id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?)?;
    let mut handoffs: Vec<HandoffReceipt> = vec![];
    let mut decisions: Vec<PendingDecision> = vec![];
    for (id, kind, actor, data) in records {
        match kind.as_str() {
            "handoff" => {
                let h: Handoff = serde_json::from_str(&data)?;
                for old in &mut handoffs {
                    if old.actor == actor && old.handoff.task == h.task {
                        old.superseded = true;
                    }
                }
                handoffs.push(HandoffReceipt {
                    id,
                    actor,
                    handoff: h,
                    superseded: false,
                    status: "unverified".into(),
                    verification_id: None,
                    verified_by: None,
                    verification: None,
                });
            }
            "verification" => {
                let v: Verification = serde_json::from_str(&data)?;
                if let Some(h) = handoffs.iter_mut().find(|h| h.id == v.handoff) {
                    h.status = v.status().into();
                    h.verification_id = Some(id);
                    h.verified_by = Some(actor);
                    h.verification = Some(v);
                }
            }
            "decision-request" => decisions.push(PendingDecision {
                id,
                actor,
                request: serde_json::from_str(&data)?,
            }),
            "decision-resolution" => {
                let d: DecisionResolution = serde_json::from_str(&data)?;
                decisions.retain(|r| r.id != d.request);
            }
            _ => (),
        }
    }
    let mut open_tasks: BTreeSet<i64> = rows(c.prepare("SELECT t.id FROM tasks t JOIN messages m USING(id) WHERE m.channel=? AND m.deleted=0 AND t.state NOT IN ('completed','canceled')")?.query_map([&run.channel],|r|r.get(0))?)?.into_iter().collect();
    for h in &handoffs {
        if let Some(id) = h.handoff.task {
            let state: String =
                c.query_row("SELECT state FROM tasks WHERE id=?", [id], |r| r.get(0))?;
            if !matches!(state.as_str(), "completed" | "canceled") {
                open_tasks.insert(id);
            }
        }
    }
    Ok(SwarmReceipt {
        run_status: run.status.clone(),
        handoffs,
        decisions,
        open_tasks: open_tasks.into_iter().collect(),
    })
}

impl Board {
    pub fn swarm_check_in(&self, actor: &str, run: &str, target: &str) -> Result<()> {
        self.swarm_check_in_with_runtime(actor, run, target, None, None, None)
    }
    pub fn swarm_check_in_with_runtime(
        &self,
        actor: &str,
        run: &str,
        target: &str,
        model: Option<&str>,
        workspace_mode: Option<&str>,
        session_ref: Option<&str>,
    ) -> Result<()> {
        for (v, max) in [(model, 128), (workspace_mode, 64), (session_ref, 1024)] {
            ensure!(
                v.is_none_or(|s| !s.trim().is_empty()
                    && s.len() <= max
                    && !s.chars().any(char::is_control)),
                Invalid.error(
                    "invalid_worker_report",
                    "expected a short, printable model, workspace mode or session reference"
                )
            );
        }
        self.write(|tx| {
            let (_,r)=self.swarm_waiting_on(tx,actor,run,Action::Post)?; r.active()?;
            let w=r.workers.iter().find(|w|w.name==target && w.actor==actor).context(Forbidden.error("wrong_swarm_actor","a worker must check itself in"))?;
            ensure!(!matches!(w.status.as_str(),"done"|"stopped"), Conflict.error("worker_stopped","worker has finished"));
            let time=crate::now();
            tx.execute("UPDATE swarm_workers SET status='working',checked_in_at=coalesce(checked_in_at,?1),last_activity_at=?1,updated_at=?1,waiter_pid=NULL,launch_error=NULL WHERE id=?2",params![time,w.id])?;
            if model.is_some() || workspace_mode.is_some() || session_ref.is_some() {
                tx.execute("INSERT INTO swarm_worker_runtime(worker_id,model,workspace_mode,session_ref) VALUES(?,?,?,?) ON CONFLICT(worker_id) DO UPDATE SET model=excluded.model,workspace_mode=excluded.workspace_mode,session_ref=excluded.session_ref",
                    params![w.id,model,workspace_mode,session_ref])?;
            }
            Board::event_tx(tx,"swarm.checked_in",actor,Some(r.root_id),json!({"worker":target,"run":r.slug}))?;
            Ok(())
        })
    }
    pub fn swarm_launch_failed(
        &self,
        actor: &str,
        run: &str,
        target: &str,
        reason: &str,
    ) -> Result<()> {
        ensure!(
            !reason.trim().is_empty() && reason.len() <= 4000,
            "provide a launch failure reason"
        );
        self.write(|tx| {
            let (_,r)=self.swarm_on(tx,actor,run,Action::Post)?; r.control()?; r.active()?;
            let w=r.workers.iter().find(|w|w.name==target).context(Missing.error("worker_not_found","unknown worker"))?;
            ensure!(matches!(w.status.as_str(),"registered"|"launch_failed"),Conflict.error("worker_started","cannot report launch failure after a worker checks in"));
            tx.execute("UPDATE swarm_workers SET status='launch_failed',launch_error=?,updated_at=? WHERE id=?",params![reason,crate::now(),w.id])?;
            Board::event_tx(tx,"swarm.launch_failed",actor,Some(r.root_id),json!({"worker":target,"reason":reason,"run":r.slug}))?;
            Ok(())
        })
    }
    pub fn swarm_receipt(&self, actor: &str, run: &str) -> Result<SwarmReceipt> {
        let tx = self.conn.unchecked_transaction()?;
        let (access, r) = self.swarm_on(&tx, actor, run, Action::Read)?;
        let mut receipt = receipt_on(&tx, &r)?;
        receipt
            .open_tasks
            .retain(|id| access.message(&tx, *id, Action::Read).is_ok());
        tx.commit()?;
        Ok(receipt)
    }
    pub fn swarm_resolve_decision(
        &self,
        actor: &str,
        run: &str,
        id: i64,
        summary: &str,
    ) -> Result<SwarmPostResult> {
        self.write(|tx| {
            let (a, r) = self.swarm_on(tx, actor, run, Action::Post)?;
            let builtin:bool=tx.query_row("SELECT coalesce((SELECT builtin FROM schema_origins WHERE name='decision-resolution' AND version=1),0)",[],|r|r.get(0))?;
            ensure!(builtin, Invalid.error("custom_decision_schema", "the existing custom decision-resolution schema is preserved; it cannot resolve built-in decisions"));
            let body = crate::append_block(
                "",
                "decision-resolution",
                1,
                &json!({"request":id,"summary":summary}),
            )?;
            self.swarm_post_on(tx, &a, &r, actor, &body, &Default::default())
        })
    }
    pub fn attention(&self, actor: &str) -> Result<Vec<AttentionItem>> {
        crate::validate_actor(actor)?;
        let tx = self.conn.unchecked_transaction()?;
        let access = self.access_on(&tx)?;
        access.actor(actor)?;
        let channels = access
            .channels(&tx, Action::Read)?
            .map(|c| serde_json::to_string(&c))
            .transpose()?;
        let mut items = rows(tx.prepare(concat!(
            "WITH selected_runs AS (SELECT id,slug,channel,root_id,status,coordinator_actor FROM swarm_runs
             WHERE ?1 IS NULL OR channel IN (SELECT value FROM json_each(?1)))",
            include_str!("current_run_actions.sql"),
            "SELECT a.kind,a.assigned_to,a.summary,a.waiting,a.source,r.slug,r.id,r.channel
             FROM current_run_actions a JOIN selected_runs r ON r.id=a.run_id
             WHERE a.kind != 'awaiting_verification' ORDER BY r.id DESC,a.kind,a.cursor"
        ))?.query_map([&channels], |r| {
            let kind: String = r.get(0)?;
            let assigned_to: String = r.get(1)?;
            let source = r.get(4)?;
            let decision = kind == "decision";
            let launch = kind == "launch_failed";
            Ok(AttentionItem {
                action_required: (decision && assigned_to == actor) || (launch &&
                    SwarmRun::may_control(&access, &r.get::<_,String>(7)?, &assigned_to, actor)),
                kind: if launch { "launch-failed".into() } else { kind },
                summary: r.get(2)?, waiting: r.get(3)?, assigned_to,
                message_id: source, decision_id: decision.then_some(source),
                run: Some(r.get(5)?), run_id: Some(r.get(6)?),
            })
        })?)?;
        let tasks=rows(tx.prepare("SELECT t.id,t.owner,t.assignee,t.result,coalesce(t.assignee,(SELECT actor FROM events e WHERE e.message_id=t.id AND e.kind IN ('task.blocked','task.failed') ORDER BY cursor DESC LIMIT 1),t.owner) FROM tasks t JOIN messages m USING(id) WHERE t.state IN ('blocked','failed') AND m.deleted=0 AND (?1 IS NULL OR m.channel IN (SELECT value FROM json_each(?1))) ORDER BY t.id")?.query_map([&channels],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,String>(4)?)))?)?;
        for (id, owner, assignee, reason, waiting) in tasks {
            let to = assignee.unwrap_or_else(|| owner.split('/').next().unwrap_or(&owner).into());
            items.push(AttentionItem {
                kind: "task".into(),
                action_required: to == actor,
                summary: reason.unwrap_or_else(|| "Task needs follow-up".into()),
                waiting,
                assigned_to: to,
                message_id: id,
                run: None,
                run_id: None,
                decision_id: None,
            });
        }
        items.sort_by_key(|i| !i.action_required);
        tx.commit()?;
        Ok(items)
    }
}
