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
    run: &SwarmRun,
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
    pub(crate) fn record_swarm_message_on(
        &self,
        tx: &Transaction,
        a: &Access,
        m: &Message,
    ) -> Result<()> {
        let blocks = coordination_blocks(tx, m)?;
        let slug: Option<String> = tx
            .query_row(
                "SELECT slug FROM swarm_runs WHERE channel=?",
                [&m.channel],
                |r| r.get(0),
            )
            .optional()?;
        let Some(slug) = slug else {
            ensure!(
                blocks.is_empty(),
                Invalid.error(
                    "swarm_required",
                    "coordination records must be posted in an existing swarm channel"
                )
            );
            return Ok(());
        };
        tx.execute("UPDATE swarm_workers SET last_activity_at=? WHERE actor=? AND run_id=(SELECT id FROM swarm_runs WHERE slug=?)",params![crate::now(),m.actor,slug])?;
        if blocks.is_empty() {
            return Ok(());
        }
        let (_, run) = self.swarm_on(tx, &m.actor, &slug, Action::Post)?;
        if blocks.iter().any(|b| b.schema != "decision-resolution") {
            run.active()?;
        }
        if let Some(target) = record_on(tx, a, &run, m)? {
            let peer = target != "coordinator" && !run.can_control;
            if peer && run.notifications_sent >= run.max_notifications {
                Board::event_tx(
                    tx,
                    "swarm.notification_suppressed",
                    &m.actor,
                    Some(m.id),
                    json!({"run":run.slug,"target":target}),
                )?;
            } else {
                super::feed::notify(tx, &run, &m.actor, &target, "mention", m.id, Some(m.id))?;
                super::feed::spend(tx, &run, &m.actor, i64::from(peer))?;
            }
        }
        Ok(())
    }
    pub fn swarm_check_in(&self, actor: &str, run: &str, target: &str) -> Result<()> {
        self.write(|tx| {
            let (_,r)=self.swarm_waiting_on(tx,actor,run,Action::Post)?; r.active()?;
            let w=r.workers.iter().find(|w|w.name==target && w.actor==actor).context(Forbidden.error("wrong_swarm_actor","a worker must check itself in"))?;
            ensure!(!matches!(w.status.as_str(),"done"|"stopped"), Conflict.error("worker_stopped","worker has finished"));
            let time=crate::now();
            tx.execute("UPDATE swarm_workers SET status='working',checked_in_at=coalesce(checked_in_at,?1),last_activity_at=?1,updated_at=?1,waiter_pid=NULL,launch_error=NULL WHERE id=?2",params![time,w.id])?;
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
        let tx = self.conn.unchecked_transaction()?;
        let access = self.access_on(&tx)?;
        access.actor(actor)?;
        let runs = rows(
            tx.prepare("SELECT slug FROM swarm_runs ORDER BY id DESC")?
                .query_map([], |r| r.get::<_, String>(0))?,
        )?;
        let mut items = vec![];
        for slug in runs {
            let Some((_, run)) = self.swarm_on(&tx, actor, &slug, Action::Read).ok() else {
                continue;
            };
            let receipt = receipt_on(&tx, &run)?;
            for d in receipt.decisions {
                items.push(AttentionItem {
                    kind: "decision".into(),
                    action_required: d.request.to == actor,
                    summary: d.request.summary,
                    waiting: d.actor,
                    assigned_to: d.request.to,
                    message_id: d.id,
                    run: Some(slug.clone()),
                    run_id: Some(run.id),
                    decision_id: Some(d.id),
                });
            }
            for w in &run.workers {
                if run.status == "active" && w.status == "launch_failed" {
                    items.push(AttentionItem {
                        kind: "launch-failed".into(),
                        action_required: run.can_control,
                        summary: w.launch_error.clone().unwrap_or_default(),
                        waiting: w.actor.clone(),
                        assigned_to: run.coordinator_actor.clone(),
                        message_id: run.root_id,
                        run: Some(slug.clone()),
                        run_id: Some(run.id),
                        decision_id: None,
                    });
                }
            }
            for h in receipt
                .handoffs
                .into_iter()
                .filter(|h| !h.superseded && matches!(h.status.as_str(), "fail" | "not-run"))
            {
                items.push(AttentionItem {
                    kind: "verification".into(),
                    action_required: false,
                    summary: h
                        .verification
                        .as_ref()
                        .map(|v| v.summary.clone())
                        .unwrap_or_default(),
                    waiting: h.verified_by.unwrap_or_default(),
                    assigned_to: h.actor,
                    message_id: h.verification_id.unwrap_or(h.id),
                    run: Some(slug.clone()),
                    run_id: Some(run.id),
                    decision_id: None,
                });
            }
        }
        let tasks=rows(tx.prepare("SELECT t.id,t.owner,t.assignee,t.result,m.channel,coalesce(t.assignee,(SELECT actor FROM events e WHERE e.message_id=t.id AND e.kind IN ('task.blocked','task.failed') ORDER BY cursor DESC LIMIT 1),t.owner) FROM tasks t JOIN messages m USING(id) WHERE t.state IN ('blocked','failed') AND m.deleted=0 ORDER BY t.id")?.query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?)))?)?;
        for (id, owner, assignee, reason, channel, waiting) in tasks {
            if !access.allows(Action::Read, &channel) {
                continue;
            }
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
