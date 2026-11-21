use super::*;
use anyhow::ensure;
use board_local::ComposerSettings;

pub struct Delivery {
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
fn scope(page: Page) -> Option<String> {
    match page {
        Page::Compose => Some("new".into()),
        Page::Thread(id) => Some(format!("thread:{id}")),
        _ => None,
    }
}
impl State {
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
        let Some(scope) = scope(self.page) else {
            return Ok(());
        };
        let channel = if self.page == Page::Compose {
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
        let Some(scope) = scope(self.page) else {
            return Ok(());
        };
        let saved = self.db()?.load_composer(&self.actor, &scope)?;
        self.draft_dirty = false;
        self.body.clear();
        self.reply_to = None;
        self.files.clear();
        self.delivery = Delivery::default();
        if let Some(d) = saved {
            self.body = TextEdit::new(d.draft.body);
            self.reply_to = d.draft.reply_to;
            self.files = d.files;
            self.delivery = Delivery::from(d.settings);
            self.delivery.stamp = Some(d.stamp);
            if self.page == Page::Compose && !d.draft.channel.is_empty() {
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
            let scope = scope(self.page).context("No active composer")?;
            self.flush_draft()?;
            let stamp = self.delivery.stamp.as_deref().context("Nothing to send")?;
            let options = PostOptions {
                files: self.files.clone(),
                no_sync: self.delivery.no_sync,
                after: self.delivery.after,
                labels: self.delivery.settings().parsed_labels()?,
                ..Default::default()
            };
            let dest = match self.page {
                Page::Compose => automation::Destination::Channel(self.destination.text.trim()),
                Page::Thread(root) => automation::Destination::Reply(self.reply_to.unwrap_or(root)),
                _ => unreachable!(),
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
                    automation::Destination::Channel(ch) => {
                        db.post_composer(ch, &self.actor, &self.body.text, &options, stamp)?
                    }
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
            Sent::Later if self.page == Page::Compose => self.go(Page::Schedules),
            Sent::Later => {
                self.error = None;
                self.refresh();
                tx.scroll_to_bottom("conversation");
            }
            Sent::Now(m) => {
                let root = m.re.unwrap_or(m.id);
                if matches!(self.page, Page::Compose) {
                    self.channel = None;
                    self.query.clear();
                    self.tasks_only = false;
                    self.starts.clear();
                    if self.db().is_ok_and(|db| db.get(root).is_ok()) {
                        self.channel = Some(m.channel);
                    } else {
                        self.go(Page::Feed);
                        return;
                    }
                }
                self.go(Page::Thread(root));
                tx.scroll_to_bottom("conversation");
            }
        }
    }
}
impl Drop for State {
    fn drop(&mut self) {
        let _ = self.flush_draft();
        self.clear_typing();
    }
}
