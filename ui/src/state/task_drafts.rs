use super::*;
use board_local::{ComposerSettings, Draft, TaskDraftSettings};

impl State {
    pub(super) fn flush_drafts(&mut self) -> Result<()> {
        // A conflict in one pane must not prevent the other from reaching SQLite.
        match (self.flush_draft(), self.flush_task_entry()) {
            (Err(message), Err(task)) => {
                anyhow::bail!("Message draft: {message:#}; task draft: {task:#}")
            }
            (message, task) => message.and(task),
        }
    }

    pub(super) fn flush_task_entry(&mut self) -> Result<()> {
        if !self.page.has_task_entry() {
            return Ok(());
        }
        let e = &self.desk.task_entry;
        let form = &e.form;
        let draft = Draft {
            body: form.body.text.clone(),
            channel: form.channel.text.trim().trim_start_matches('#').to_owned(),
            reply_to: None,
        };
        let task = TaskDraftSettings {
            needs: form.extra.text.clone(),
            assignee: form.name.text.clone(),
        };
        let present = !draft.body.is_empty()
            || (!draft.channel.is_empty() && draft.channel != "general")
            || !e.files.is_empty()
            || !e.labels.text.is_empty()
            || e.no_sync
            || task != TaskDraftSettings::default();
        let state = present.then(|| {
            (
                draft,
                ComposerSettings {
                    no_sync: e.no_sync,
                    labels: e.labels.text.clone(),
                    task: Some(task),
                    ..Default::default()
                },
            )
        });
        if !e.files_changed && state == e.baseline {
            return Ok(());
        }
        let result = self.db()?.save_composer(
            &self.actor,
            "tasks",
            e.stamp.as_deref(),
            state.as_ref().map(|(d, s)| (d, s)),
            e.files_changed.then_some(e.files.as_slice()),
        );
        let entry = &mut self.desk.task_entry;
        if let Err(ref error) = result
            && board_local::errors::classify(error).1 == "draft_changed"
        {
            entry.reload_needed = true;
        }
        entry.stamp = result?;
        entry.baseline = state;
        entry.files_changed = false;
        entry.reload_needed = false;
        Ok(())
    }

    pub(super) fn restore_task_entry(&mut self) -> Result<()> {
        let saved = self.db()?.load_composer(&self.actor, "tasks")?;
        let mut entry = workspace::TaskEntry::default();
        if let Some(saved) = saved {
            let settings = &saved.settings;
            let task = settings
                .task
                .as_ref()
                .context("Missing saved task routing")?;
            entry.form.body = TextEdit::new(&saved.draft.body);
            entry.form.channel = TextEdit::new(&saved.draft.channel);
            entry.form.name = TextEdit::new(&task.assignee);
            entry.form.extra = TextEdit::new(&task.needs);
            entry.labels = TextEdit::new(&settings.labels);
            entry.no_sync = settings.no_sync;
            entry.stamp = Some(saved.stamp);
            entry.files = saved.files;
            entry.baseline = Some((saved.draft, saved.settings));
        } else if self.page == Page::NewTask {
            entry.form.channel = TextEdit::new("general");
        }
        self.desk.task_entry = entry;
        Ok(())
    }

    pub fn reload_task_entry(&mut self) {
        match self.restore_task_entry() {
            Ok(()) => self.error = None,
            Err(e) => {
                self.error = Some(format!("Task draft not available: {e:#}"));
                self.refresh();
            }
        }
    }
}
