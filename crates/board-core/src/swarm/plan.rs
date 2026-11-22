use super::*;
use crate::automation::{PlanItem, Prerequisite, create_plan_guarded};
use sha2::{Digest, Sha256};

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
    pub(super) fn validate(self) -> Result<()> {
        ensure!(
            (1..=128).contains(&self.max_active) && (0..=1000).contains(&self.retry_budget),
            Invalid.error(
                "invalid_policy",
                "max_active must be 1..128 and retry_budget 0..1000"
            )
        );
        Ok(())
    }
    pub(super) fn read(c: &Connection, id: i64) -> Result<Self> {
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

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SwarmPlanItem {
    pub key: String,
    pub text: String,
    pub acceptance: String,
    pub output: String,
    #[serde(default)]
    pub needs: Vec<Prerequisite>,
    pub assignee: Option<String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct PlanReceipt {
    pub run: String,
    pub key: String,
    pub tasks: BTreeMap<String, i64>,
}
impl Board {
    pub fn swarm_plan(
        &self,
        actor: &str,
        run: &str,
        key: &str,
        items: &[SwarmPlanItem],
    ) -> Result<PlanReceipt> {
        ensure!(
            valid_slug(key, 64),
            Invalid.error("invalid_plan", "use a short lowercase plan key")
        );
        ensure!(
            (1..=4096).contains(&items.len()),
            Invalid.error("invalid_plan", "a plan requires 1..4096 items")
        );
        let (_, r) = self.swarm_record(&self.conn, actor, run, Action::Post)?;
        r.control()?;
        let mut tasks = Vec::with_capacity(items.len());
        for i in items {
            ensure!(
                valid_slug(&i.key, 64)
                    && [&i.text, &i.acceptance, &i.output]
                        .iter()
                        .all(|s| !s.trim().is_empty()),
                Invalid.error(
                    "invalid_plan",
                    "each item needs a key, text, acceptance criteria and expected output"
                )
            );
            let mut labels = i.labels.clone();
            ensure!(
                labels.insert("swarm.plan".into(), key.into()).is_none()
                    && labels.insert("swarm.item".into(), i.key.clone()).is_none(),
                Invalid.error(
                    "invalid_plan",
                    "swarm.plan and swarm.item are reserved labels"
                )
            );
            let text = format!(
                "{}\n\nAcceptance: {}\nExpected output: {}",
                i.text, i.acceptance, i.output
            );
            let options = PostOptions {
                labels,
                no_sync: true,
                ..Default::default()
            };
            crate::validate_post(&r.channel, actor, &text, &options)?;
            tasks.push(PlanItem {
                key: Some(i.key.clone()),
                text,
                labels: options.labels,
                needs: i.needs.clone(),
                assignee: i.assignee.clone(),
                attachment_ids: vec![],
                no_sync: true,
            });
        }
        let digest = Sha256::digest(serde_json::to_vec(items)?);
        let batch = format!("swarm-plan:{}:{key}", r.id);
        let (made, _) = create_plan_guarded(self, actor, &r.channel, &tasks, Some(&batch), |tx| {
            // Recheck on every checkpoint: a stop, credential revocation, or competing
            // submission can land between chunks of a long plan.
            let (_, r) = self.swarm_record(tx, actor, run, Action::Post)?;
            r.control()?;
            if r.status == "active" {
                tx.execute(
                    "INSERT INTO swarm_plans VALUES(?,?,?,?) ON CONFLICT(run_id,key) DO NOTHING",
                    params![r.id, key, actor, &digest[..]],
                )?;
            } else {
                // Completed submissions remain replayable, but stopping prevents
                // new submissions and additional checkpoints of partial ones.
                ensure!(tx.query_row("SELECT EXISTS(SELECT 1 FROM client_batches WHERE key=? AND actor=? AND json_array_length(ids)=?) AND NOT EXISTS(SELECT 1 FROM plan_progress WHERE key=?)",params![batch,actor,items.len(),batch],|r|r.get::<_,bool>(0))?,
                        Conflict.error("swarm_stopped","the swarm has ended"));
            }
            ensure!(
                tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM swarm_plans WHERE actor=? AND digest=? AND run_id=? AND key=?)",
                    params![actor, &digest[..], r.id, key],
                    |r| r.get::<_, bool>(0)
                )?,
                Conflict.error(
                    "duplicate_id",
                    "that plan key belongs to another submission"
                )
            );
            Ok(())
        })?;
        Ok(PlanReceipt {
            run: r.slug,
            key: key.into(),
            tasks: items
                .iter()
                .zip(made)
                .map(|(i, t)| (i.key.clone(), t.id))
                .collect(),
        })
    }
}
