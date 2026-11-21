use super::*;
use anyhow::ensure;
use board_local::ComposerSettings;

pub struct Delivery {
    // A pending native file dialog belongs to one mounted composer, even on the same thread.
    pub(super) instance: std::sync::Arc<()>,
    pub open: bool,
    pub no_sync: bool,
    pub later: bool,
    pub after: Option<i64>,
    pub due: TextEdit,
    pub every: TextEdit,
    pub times: TextEdit,
    pub until: TextEdit,
    pub labels: TextEdit,
    pub stamp: Option<String>,
    files_dirty: bool,
}
impl From<ComposerSettings> for Delivery {
    fn from(d: ComposerSettings) -> Self {
        Self {
            instance: std::sync::Arc::new(()),
            open: false,
            no_sync: d.no_sync,
            later: d.later,
            after: d.after,
            due: TextEdit::new(d.due),
            every: TextEdit::new(d.every),
            times: TextEdit::new(d.times),
            until: TextEdit::new(d.until),
            labels: TextEdit::new(d.labels),
            stamp: None,
            files_dirty: false,
        }
    }
}
impl Default for Delivery {
    fn default() -> Self {
        ComposerSettings::default().into()
    }
}
impl Delivery {
    fn settings(&self) -> ComposerSettings {
        ComposerSettings {
            no_sync: self.no_sync,
            later: self.later,
            after: self.after,
            due: self.due.text.clone(),
            every: self.every.text.clone(),
            times: self.times.text.clone(),
            until: self.until.text.clone(),
            labels: self.labels.text.clone(),
            task: None,
        }
    }
    fn timing(&self) -> Result<automation::Timing> {
        let timing = self.settings().parsed_timing()?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        ensure!(
            timing.due > now,
            "The draft's send time has passed; choose a future time or explicitly turn off scheduling"
        );
        Ok(timing)
    }
}
impl State {
    pub fn edits_root(&self) -> bool {
        self.page.is_feed() || (self.root_editor_in_thread && matches!(self.page, Page::Thread(_)))
    }
    pub(super) fn root_scope(&self) -> String {
        if self.page != Page::Compose
            && let Some(ch) = &self.channel
        {
            format!("channel:{}", ch.trim_end_matches('/'))
        } else {
            "new".into()
        }
    }
    pub(super) fn draft_scope(&self) -> Option<String> {
        if self.edits_root() {
            return Some(self.root_scope());
        }
        self.page.thread_id().map(|id| format!("thread:{id}"))
    }

    pub fn set_task_options(&mut self, open: bool) {
        if !matches!(self.page, Page::Task(_)) || self.desk.task_options == open {
            return;
        }
        if open && let Err(e) = self.flush_draft() {
            self.error = Some(format!("Draft not saved: {e:#}"));
            return;
        }
        // The options replace the task's editor. Never complete a dialog opened on
        // the previous mount, even if the user has returned to the same thread.
        self.delivery.instance = std::sync::Arc::new(());
        self.clear_typing();
        self.desk.task_options = open;
    }

    pub(super) fn hide_unavailable_draft(&mut self) {
        // Leave the sealed SQLite copy alone. A narrower grant may still be
        // valid for the rest of the board, but not for this saved destination.
        self.body.clear();
        self.people.inline = Default::default();
        self.files.clear();
        self.destination.clear();
        self.reply_to = None;
        self.delivery = Delivery::default();
        self.draft_dirty = false;
        self.error = Some("This saved draft is no longer visible to the active credential".into());
    }
    pub fn activate_draft(&mut self, root: bool) -> bool {
        if self.access_blocked {
            return false;
        }
        if self.edits_root() == root {
            return true;
        }
        if !matches!(self.page, Page::Thread(_)) {
            return false;
        }
        if let Err(e) = self.flush_draft() {
            self.error = Some(format!("Draft not saved: {e:#}"));
            return false;
        }
        self.clear_typing();
        self.root_editor_in_thread = root;
        if let Err(e) = self.restore_draft() {
            // restore_draft reads before changing the active fields, so failure leaves
            // the previously displayed editor and its stamp intact.
            self.root_editor_in_thread = !root;
            self.error = Some(format!("Draft not available: {e:#}"));
            return false;
        }
        self.properties = None;
        self.people = People::default();
        self.error = None;
        self.refresh();
        true
    }
    pub fn draft_changed(&mut self) {
        self.draft_dirty = true;
    }
    pub fn files_changed(&mut self) {
        self.delivery.files_dirty = true;
        self.draft_changed();
    }
    pub fn open_delivery(&mut self) {
        self.delivery.open = true;
    }
    pub fn guard_reply(&mut self, id: i64, tx: &mut Tx) {
        if !self.activate_draft(false) {
            return;
        }
        if self.delivery.later {
            self.error = Some("Turn off scheduled delivery before pinning a guarded reply".into());
            return;
        }
        self.delivery.after = Some(id);
        self.reply_to = Some(id);
        self.draft_changed();
        self.properties = None;
        self.error = None;
        tx.focus_when_shown("message-body", true);
    }
    pub fn remove_guard(&mut self) {
        self.delivery.after = None;
        self.draft_changed();
        self.error = None;
    }
    pub fn flush_draft(&mut self) -> Result<()> {
        if !self.draft_dirty {
            return Ok(());
        }
        let Some(scope) = self.draft_scope() else {
            return Ok(());
        };
        let channel = if self.edits_root() {
            self.destination.text.trim().to_owned()
        } else {
            self.thread
                .first()
                .map_or_else(String::new, |m| m.channel.clone())
        };
        let settings = self.delivery.settings();
        let d = board_local::Draft {
            body: self.body.text.clone(),
            channel,
            reply_to: self.reply_to,
        };
        let present = !d.body.is_empty()
            || !self.files.is_empty()
            || settings != ComposerSettings::default()
            || self.reply_to.is_some();
        self.delivery.stamp = self.db()?.save_composer(
            &self.actor,
            &scope,
            self.delivery.stamp.as_deref(),
            present.then_some((&d, &settings)),
            self.delivery.files_dirty.then_some(self.files.as_slice()),
        )?;
        self.delivery.files_dirty = false;
        self.draft_dirty = false;
        Ok(())
    }
    pub fn restore_draft(&mut self) -> Result<()> {
        let Some(scope) = self.draft_scope() else {
            return Ok(());
        };
        let saved = self.db()?.load_composer(&self.actor, &scope)?;
        self.people.inline = Default::default();
        self.draft_dirty = false;
        self.body.clear();
        self.reply_to = None;
        self.files.clear();
        self.delivery = Delivery::default();
        if self.edits_root() {
            self.destination = TextEdit::new(
                self.channel
                    .as_deref()
                    .unwrap_or("general")
                    .trim_end_matches('/'),
            );
        }
        if let Some(d) = saved {
            self.body = TextEdit::new(d.draft.body);
            self.reply_to = d.draft.reply_to;
            self.files = d.files;
            self.delivery = Delivery::from(d.settings);
            self.delivery.stamp = Some(d.stamp);
            if self.edits_root() && !d.draft.channel.is_empty() {
                self.destination = TextEdit::new(d.draft.channel);
            }
        }
        Ok(())
    }
    pub fn reload_composer(&mut self) {
        match self.restore_draft() {
            Ok(()) => self.error = None,
            Err(e) => self.error = Some(e.to_string()),
        }
    }
    pub fn send(&mut self, tx: &mut Tx) {
        enum Sent {
            Now(Box<Message>),
            Later,
        }
        let mut run = || -> Result<Sent> {
            let scope = self.draft_scope().context("No active composer")?;
            self.flush_draft()?;
            let stamp = self.delivery.stamp.as_deref().context("Nothing to send")?;
            let options = PostOptions {
                files: self.files.clone(),
                no_sync: self.delivery.no_sync,
                after: self.delivery.after,
                labels: self.delivery.settings().parsed_labels()?,
                ..Default::default()
            };
            let dest = if self.edits_root() {
                automation::Destination::Channel(self.destination.text.trim())
            } else if let Some(root) = self.page.thread_id() {
                automation::Destination::Reply(self.reply_to.unwrap_or(root))
            } else {
                anyhow::bail!("No active composer")
            };
            let db = self.db()?;
            if self.delivery.later {
                automation::schedule_draft(
                    db,
                    &self.actor,
                    dest,
                    &self.body.text,
                    self.delivery.timing()?,
                    &options,
                    (&scope, stamp),
                )?;
                Ok(Sent::Later)
            } else {
                let m = match dest {
                    automation::Destination::Channel(ch) => db.post_scoped_composer(
                        ch,
                        &self.actor,
                        &self.body.text,
                        &options,
                        (&scope, stamp),
                    )?,
                    automation::Destination::Reply(id) => db.reply_composer(
                        id,
                        &self.actor,
                        &self.body.text,
                        &options,
                        (&scope, stamp),
                    )?,
                };
                Ok(Sent::Now(Box::new(m)))
            }
        };
        let outcome = match run() {
            Ok(o) => o,
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                return;
            }
        };
        self.clear_typing();
        self.body.clear();
        self.files.clear();
        self.reply_to = None;
        self.draft_dirty = false;
        self.delivery = Delivery::default();
        self.browse.reset_thread();
        match outcome {
            Sent::Later if self.edits_root() => self.go(Page::Schedules),
            Sent::Later => {
                self.error = None;
                self.refresh();
                tx.scroll_to_bottom("conversation");
            }
            Sent::Now(m) => {
                let root = m.re.unwrap_or(m.id);
                if self.edits_root() {
                    self.channel = None;
                    self.query.clear();
                    self.tasks_only = false;
                    self.reset_feed();
                    if self.db().is_ok_and(|db| db.get(root).is_ok()) {
                        self.channel = Some(m.channel);
                    } else {
                        self.go(Page::Feed);
                        return;
                    }
                }
                if self.page == Page::Task(root) {
                    self.error = None;
                    self.refresh();
                } else {
                    self.go(Page::Thread(root));
                }
                tx.scroll_to_bottom("conversation");
            }
        }
    }
}
impl Drop for State {
    fn drop(&mut self) {
        let _ = self.flush_drafts();
        self.clear_typing();
    }
}
