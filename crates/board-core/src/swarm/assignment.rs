use super::*;
use crate::roles::{PinnedSkill, SWARM_WORKER_PROTOCOL};

#[derive(Serialize)]
pub struct Assignment {
    pub run: String,
    pub channel: String,
    pub goal: String,
    pub worker: SwarmWorker,
    pub protocol: &'static str,
    pub role_prompt: String,
    pub skills: Vec<AssignmentSkill>,
}

#[derive(Serialize)]
pub struct AssignmentSkill {
    pub slug: String,
    pub version: i64,
    pub files: Vec<String>,
}

#[derive(Serialize)]
pub struct AssignmentFile {
    pub skill: String,
    pub version: i64,
    pub path: String,
    pub text: String,
}

impl Board {
    pub(super) fn pinned_assignment(c: &Connection, id: i64) -> Result<(String, Vec<PinnedSkill>)> {
        let (prompt, json): (String, String) = c.query_row(
            "SELECT pinned_system_prompt,pinned_skills_json FROM swarm_workers WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((prompt, serde_json::from_str(&json)?))
    }

    fn own_worker(
        &self,
        c: &Connection,
        binding: &WorkerBinding,
    ) -> Result<(SwarmRun, SwarmWorker)> {
        let (_, run) = self.swarm_record(c, &binding.actor, &binding.run, Action::Read)?;
        let worker = c
            .query_row(
                &format!("{WORKER_SELECT} WHERE w.run_id=? AND w.actor=? AND w.name=?"),
                params![run.id, binding.actor, binding.name],
                worker,
            )
            .optional()?
            .filter(|_| run.id == binding.run_id)
            .context(Forbidden.error("worker_identity_mismatch", "not your assignment"))?;
        Ok((run, worker))
    }

    /// Only the pinned copy is shared with a bound worker, never the owner's role library.
    pub fn worker_assignment(&self, binding: &WorkerBinding) -> Result<Assignment> {
        let tx = self.conn.unchecked_transaction()?;
        let (r, worker) = self.own_worker(&tx, binding)?;
        let role_prompt = tx.query_row(
            "SELECT pinned_system_prompt FROM swarm_workers WHERE id=?",
            [worker.id],
            |r| r.get(0),
        )?;
        // Send the index, not every file body, when a tools-only worker checks its assignment.
        let index = rows(
            tx.prepare(
                "SELECT s.value->>'slug',s.value->>'version',
                (SELECT json_group_array(f.key) FROM json_each(s.value,'$.files') f)
            FROM swarm_workers w, json_each(w.pinned_skills_json) s WHERE w.id=? ORDER BY s.key",
            )?
            .query_map([worker.id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?))
            })?,
        )?;
        let skills = index
            .into_iter()
            .map(|(slug, version, names)| {
                Ok(AssignmentSkill {
                    slug,
                    version,
                    files: serde_json::from_str(&names)?,
                })
            })
            .collect::<Result<_>>()?;
        tx.commit()?;
        Ok(Assignment {
            run: r.slug,
            channel: r.channel,
            goal: r.goal,
            worker,
            protocol: SWARM_WORKER_PROTOCOL,
            role_prompt,
            skills,
        })
    }

    pub fn worker_skill_file(
        &self,
        binding: &WorkerBinding,
        skill: &str,
        path: &str,
    ) -> Result<AssignmentFile> {
        let tx = self.conn.unchecked_transaction()?;
        let (_, w) = self.own_worker(&tx, binding)?;
        // Match stored names literally. Neither argument ever reaches the filesystem.
        let (version, text) = tx
            .query_row(
                "SELECT s.value->>'version', f.value FROM swarm_workers w,
                json_each(w.pinned_skills_json) s, json_each(s.value, '$.files') f
             WHERE w.id=? AND s.value->>'slug'=? AND f.key=?",
                params![w.id, skill, path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .context(Missing.error("assignment_file_not_found", "file not in this assignment"))?;
        tx.commit()?;
        Ok(AssignmentFile {
            skill: skill.into(),
            version,
            path: path.into(),
            text,
        })
    }
}
