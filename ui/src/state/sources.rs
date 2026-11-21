use anyhow::{Context, Result};
use board_local::{
    Board,
    sources::{Direction, Job, Source},
};

pub struct Entry {
    pub id: i64,
    pub state: String,
    pub attempts: i64,
    pub operation: &'static str,
    pub local: Option<i64>,
    pub error_recorded: bool,
}
impl From<Job> for Entry {
    fn from(job: Job) -> Self {
        let verb = job
            .payload
            .pointer("/change/op")
            .or_else(|| job.payload.get("event"))
            .and_then(|v| v.as_str());
        let operation = match verb {
            Some("post" | "message.posted") => "Post",
            Some("upsert") => "Update or post",
            Some("edit" | "message.edited") => "Edit",
            Some("delete" | "message.deleted") => "Delete",
            Some("labels" | "labels.changed") => "Labels",
            Some("react" | "reaction.added" | "reaction.removed") => "Reaction",
            Some("thread.start") => "Start thread",
            _ => "Change",
        };
        let local = job.result.or_else(|| {
            (job.direction == Direction::Out)
                .then(|| job.payload.get("message").and_then(|v| v.as_i64()))
                .flatten()
        });
        // Raw requests, receipt tokens, remote URLs and arbitrary error strings do not
        // belong in the element tree or its accessibility export.
        Self {
            id: job.id,
            state: job.state,
            attempts: job.attempts,
            operation,
            local,
            error_recorded: job.error.is_some(),
        }
    }
}

#[derive(Default)]
pub struct Review {
    pub list: Vec<Source>,
    pub selected: String,
    pub outgoing: bool,
    pub rows: Vec<Entry>,
    pub expanded: Option<i64>,
    pub copies: usize,
}
impl Review {
    pub fn clear_data(&mut self) {
        self.list.clear();
        self.rows.clear();
        self.copies = 0;
    }
    pub fn load(
        &mut self,
        db: &Board,
        actor: &str,
        after: i64,
        limit: usize,
    ) -> Result<Option<i64>> {
        let config = db.source(actor, &self.selected)?;
        let dir = if self.outgoing {
            Direction::Out
        } else {
            Direction::In
        };
        let rows = db.inbox(actor, &self.selected, dir, after, limit + 1)?;
        let next = if rows.len() > limit {
            Some(rows.get(limit - 1).context("empty source page")?.id)
        } else {
            None
        };
        self.list = vec![config];
        self.rows = rows.into_iter().take(limit).map(Entry::from).collect();
        self.copies = if let Some(local) = self
            .rows
            .iter()
            .find(|r| Some(r.id) == self.expanded)
            .and_then(|r| r.local)
        {
            db.source_copies(actor, &self.selected, local)?.len()
        } else {
            0
        };
        Ok(next)
    }
}
