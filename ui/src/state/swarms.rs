use super::{Page, State};
use anyhow::Result;
use board_local::{
    Board,
    access::Action,
    swarm::{SwarmEntry, SwarmFilter, SwarmObservation, SwarmRun, SwarmSummary, SwarmWorker},
};
use rune::{TextEdit, Tx};
use std::collections::HashSet;

const WINDOW: usize = 24;
#[derive(Default, Clone, Copy, Eq, PartialEq)]
pub enum FeedKind {
    #[default]
    All,
    Public,
    Direct,
    Addressed,
}
#[derive(Default, Clone, Copy, Eq, PartialEq)]
pub enum SwarmTab {
    #[default]
    Overview,
    Conversation,
    Receipt,
}
#[derive(Default, Clone, Eq, PartialEq)]
pub enum SwarmTarget {
    #[default]
    Public,
    Direct(String),
}
#[derive(Default)]
pub struct Observer {
    pub receipt: Option<board_local::swarm::SwarmReceipt>,
    pub tab: SwarmTab,
    pub overview: Option<SwarmObservation>,
    pub task_starts: Vec<i64>,
    pub action_starts: Vec<i64>,
    pub public_seen: i64,
    pub retired_open: bool,
    pub runs: Vec<SwarmSummary>,
    pub run: Option<SwarmRun>,
    pub entries: Vec<SwarmEntry>,
    pub run_starts: Vec<i64>,
    pub older_run: Option<i64>,
    pub feed_starts: Vec<i64>,
    pub older_post: Option<i64>,
    pub kind: FeedKind,
    pub worker: Option<String>,
    pub briefs: HashSet<String>,
    pub offline: HashSet<i64>,
    pub roster_open: bool,
    pub choose_target: bool,
    pub choose_run: bool,
    pub target: SwarmTarget,
    pub body: TextEdit,
    pub can_send: bool,
    pub confirm_stop: bool,
}
#[cfg(unix)]
fn stale_waiter(worker: &SwarmWorker) -> bool {
    use rustix::{
        io::Errno,
        process::{Pid, test_kill_process},
    };
    if worker.status != "waiting" {
        return false;
    }
    let Some(pid) = worker
        .waiter_pid
        .and_then(|n| i32::try_from(n).ok())
        .and_then(Pid::from_raw)
    else {
        return false;
    };
    let Ok(updated) = chrono::DateTime::parse_from_rfc3339(&worker.updated_at) else {
        return false;
    };
    chrono::Utc::now()
        .signed_duration_since(updated)
        .num_seconds()
        >= 5
        && test_kill_process(pid) == Err(Errno::SRCH)
}
#[cfg(not(unix))]
fn stale_waiter(_: &SwarmWorker) -> bool {
    false
}
impl Observer {
    pub fn load(&mut self, db: &Board, actor: &str, page: Page) -> Result<()> {
        let mut runs = db.swarm_summaries(actor, self.run_starts.last().copied(), WINDOW + 1)?;
        self.older_run = (runs.len() > WINDOW).then(|| runs[WINDOW - 1].id);
        runs.truncate(WINDOW);
        self.runs = runs;
        if let Page::Swarm(id) = page {
            let key = format!("#{id}");
            let run = db.swarm_status(actor, &key)?;
            if self.run.as_ref().is_some_and(|old| old.id != run.id) {
                self.tab = SwarmTab::Overview;
                self.task_starts.clear();
                self.action_starts.clear();
            }
            self.overview = (self.tab == SwarmTab::Overview)
                .then(|| {
                    db.swarm_observe_page(
                        actor,
                        &key,
                        None,
                        self.action_starts.last().copied().unwrap_or(0),
                        self.task_starts.last().copied().unwrap_or(0),
                        WINDOW,
                    )
                })
                .transpose()?;
            self.receipt = (self.tab == SwarmTab::Receipt)
                .then(|| db.swarm_receipt(actor, &key))
                .transpose()?;
            self.entries.clear();
            self.older_post = None;
            if self.tab == SwarmTab::Conversation {
                self.public_seen = db.swarm_public_seen(actor, &key)?;
                let mut page = db.swarm_latest_filtered(
                    actor,
                    &key,
                    self.feed_starts.last().copied(),
                    WINDOW + 1,
                    true,
                    &SwarmFilter {
                        kind: match self.kind {
                            FeedKind::All => None,
                            FeedKind::Public => Some("public".into()),
                            FeedKind::Direct => Some("dm".into()),
                            FeedKind::Addressed => Some("addressed".into()),
                        },
                        worker: self.worker.clone(),
                    },
                )?;
                if page.entries.len() > WINDOW {
                    page.entries.remove(0);
                    self.older_post = page.entries.first().map(|p| p.cursor);
                } else {
                    self.older_post = None;
                }
                self.entries = page.entries;
            }
            self.can_send = run.status == "active"
                && (db.viewer(actor)?.owner
                    || db.explain(actor, &run.channel)?.contains(&Action::Post));
            if !self.can_send {
                self.confirm_stop = false;
            }
            self.run = Some(run);
            self.refresh_waiters();
        }
        Ok(())
    }
    pub fn refresh_waiters(&mut self) -> bool {
        let Some(run) = self.run.as_ref() else {
            return false;
        };
        let offline = run
            .workers
            .iter()
            .filter(|w| stale_waiter(w))
            .map(|w| w.id)
            .collect();
        if self.offline == offline {
            return false;
        }
        self.offline = offline;
        true
    }
}
impl State {
    pub fn swarm_tab(&mut self, tab: SwarmTab) {
        self.desk.swarm.tab = tab;
        self.refresh();
    }
    pub fn mark_swarm_seen(&mut self, through: i64) {
        let Page::Swarm(id) = self.page else { return };
        let result = self
            .db()
            .and_then(|db| db.swarm_mark_public_seen(&self.actor, &format!("#{id}"), through));
        if self.result(result).is_some() {
            self.refresh();
        }
    }
    pub fn begin_decision_answer(&mut self, id: i64) {
        if self.desk.selected_decision == Some(id) {
            return;
        }
        self.remember_decision_answer();
        self.desk.selected_decision = Some(id);
        self.desk.decision_answer = TextEdit::new(
            self.desk
                .decision_drafts
                .get(&id)
                .map(String::as_str)
                .unwrap_or(""),
        );
    }
    fn remember_decision_answer(&mut self) {
        if let Some(id) = self.desk.selected_decision {
            self.desk
                .decision_drafts
                .insert(id, self.desk.decision_answer.text.clone());
        }
    }
    pub fn cancel_decision_answer(&mut self) {
        self.remember_decision_answer();
        self.desk.selected_decision = None;
        self.desk.decision_answer.clear();
    }
    pub fn answer_decision(&mut self) {
        let Some(id) = self.desk.selected_decision else {
            return;
        };
        let Some(run) = self
            .desk
            .attention
            .iter()
            .find(|i| i.decision_id == Some(id))
            .and_then(|i| i.run.clone())
        else {
            return;
        };
        let summary = self.desk.decision_answer.text.clone();
        if summary.trim().is_empty() {
            return;
        }
        let result = self
            .db()
            .and_then(|b| b.swarm_resolve_decision(&self.actor, &run, id, &summary));
        if self.result(result).is_some() {
            self.desk.decision_drafts.remove(&id);
            self.desk.selected_decision = None;
            self.desk.decision_answer.clear();
            self.refresh();
        }
    }
    pub fn refresh_swarm_filter(&mut self) {
        self.desk.swarm.tab = SwarmTab::Conversation;
        self.desk.swarm.feed_starts.clear();
        self.refresh();
    }
    pub fn swarm_send(&mut self, tx: &mut Tx) {
        let Page::Swarm(id) = self.page else { return };
        if !self.desk.swarm.can_send || self.desk.swarm.body.text.trim().is_empty() {
            return;
        }
        let result = self.db().and_then(|db| {
            let o = &self.desk.swarm;
            let run = format!("#{id}");
            match &o.target {
                SwarmTarget::Public => {
                    db.swarm_post(&self.actor, &run, &o.body.text, &Default::default())?;
                }
                SwarmTarget::Direct(to) => {
                    db.swarm_dm(&self.actor, &run, to, &o.body.text)?;
                }
            }
            Ok(())
        });
        if self.result(result).is_some() {
            self.desk.swarm.body.clear();
            self.desk.swarm.feed_starts.clear();
            self.refresh();
            tx.focus_when_shown("swarm-body", true);
            tx.scroll_to_bottom("swarm-feed");
        }
    }
    pub fn swarm_stop(&mut self) {
        let Page::Swarm(id) = self.page else { return };
        if !self.desk.swarm.confirm_stop {
            self.desk.swarm.confirm_stop = true;
            return;
        }
        self.desk.swarm.confirm_stop = false;
        let result = self
            .db()
            .and_then(|db| db.swarm_stop(&self.actor, &format!("#{id}")));
        if self.result(result).is_some() {
            self.swarm_tab(SwarmTab::Receipt);
        }
    }
    pub fn swarm_page(&mut self, runs: bool, older: bool) {
        let o = &mut self.desk.swarm;
        let (stack, cursor) = if runs {
            (&mut o.run_starts, o.older_run)
        } else {
            (&mut o.feed_starts, o.older_post)
        };
        if older {
            if let Some(c) = cursor {
                stack.push(c);
            }
        } else {
            stack.pop();
        }
        self.refresh();
    }
}
