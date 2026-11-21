use super::*;

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
    pub(super) fn flush_draft(&mut self) -> Result<()> {
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
        let draft = (!self.body.text.is_empty()).then(|| board_local::Draft {
            body: self.body.text.clone(),
            channel,
            reply_to: self.reply_to,
        });
        self.db()?.save_draft(&self.actor, &scope, draft.as_ref())?;
        self.draft_dirty = false;
        Ok(())
    }
    pub(super) fn restore_draft(&mut self, unsaved: bool) -> Result<()> {
        self.draft_dirty = false;
        let Some(scope) = scope(self.page) else {
            return Ok(());
        };
        let stored = self.db()?.load_draft(&self.actor, &scope)?;
        if unsaved {
            self.draft_dirty = true;
        } else if let Some(d) = stored {
            self.body = TextEdit::new(d.body);
            self.reply_to = d.reply_to;
            if self.page == Page::Compose && !d.channel.is_empty() {
                self.destination = TextEdit::new(d.channel);
            }
        } else if !self.body.text.is_empty() {
            self.body.clear();
            self.reply_to = None;
        }
        Ok(())
    }
}
impl Drop for State {
    fn drop(&mut self) {
        let _ = self.flush_draft();
        self.clear_typing();
    }
}
