use super::{Page, State};
use anyhow::{Context, Result, ensure};
use board_local::{
    Board,
    access::Action,
    sources::{Direction, Job, Provider, Source, SourcePatch},
};
use rune::{TextEdit, Tx};

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
            Some("labels" | "labels.changed") => "Source details",
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
    pub can_copy_command: bool,
    pub can_add: bool,
    pub creating: bool,
    pub mapping: Mapping,
    pub edit: Option<Settings>,
    pub notice: Option<String>,
}
pub struct Mapping {
    pub name: TextEdit,
    pub remote: TextEdit,
    pub channel: TextEdit,
    pub provider: Provider,
}
impl Default for Mapping {
    fn default() -> Self {
        Self {
            name: TextEdit::new(""),
            remote: TextEdit::new(""),
            channel: TextEdit::new(""),
            provider: Provider::Local,
        }
    }
}
pub struct Settings {
    pub original: Source,
    pub draft: Source,
    pub confirm: bool,
}
impl Review {
    pub fn command(&self) -> String {
        self.list
            .first()
            .map(|s| format!("board {}", source_args(s)))
            .unwrap_or_default()
    }
    pub fn clear_data(&mut self) {
        self.list.clear();
        self.rows.clear();
        self.copies = 0;
        self.can_copy_command = false;
        self.can_add = false;
        self.creating = false;
        self.mapping = Mapping::default();
        self.edit = None;
        self.notice = None;
    }
    pub fn listing(&mut self, db: &Board, actor: &str) -> Result<()> {
        let viewer = db.viewer(actor)?;
        self.can_add =
            viewer.owner
                || viewer.can.0.values().any(|actions| {
                    actions.contains(&Action::Read) && actions.contains(&Action::Sync)
                });
        self.list = if self.can_add {
            match db.sources(actor) {
                Ok(sources) => sources,
                Err(e) if board_local::errors::classify(&e).1 == "narrowed" => {
                    self.can_add = false;
                    Vec::new()
                }
                Err(e) => return Err(e),
            }
        } else {
            Vec::new()
        };
        if !self.can_add {
            self.creating = false;
            self.mapping = Mapping::default();
            self.notice = None;
        }
        Ok(())
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
        self.can_copy_command = quoted_database(db).is_some();
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

impl State {
    pub fn begin_source(&mut self) {
        if self.page == Page::Sources && self.desk.source.can_add {
            self.desk.source.mapping = Mapping::default();
            self.desk.source.creating = true;
            self.desk.source.notice = None;
        }
    }
    pub fn cancel_source_form(&mut self) {
        self.desk.source.creating = false;
        self.desk.source.mapping = Mapping::default();
        self.desk.source.edit = None;
        self.desk.source.notice = None;
    }
    pub fn create_source_mapping(&mut self) {
        if self.page != Page::Sources || !self.desk.source.creating {
            return;
        }
        let form = &self.desk.source.mapping;
        let result = self.db().and_then(|db| {
            db.create_source(
                &self.actor,
                form.name.text.trim(),
                form.provider.clone(),
                form.remote.text.trim(),
                form.channel.text.trim(),
            )
        });
        match result {
            Ok((source, _)) => {
                self.desk.source.selected = source.name;
                self.desk.source.outgoing = false;
                self.go(Page::Inbox);
            }
            Err(e) => self.desk.source.notice = Some(e.to_string()),
        }
    }
    pub fn edit_source_settings(&mut self) {
        if self.page != Page::Inbox {
            return;
        }
        match self
            .db()
            .and_then(|b| b.source(&self.actor, &self.desk.source.selected))
        {
            Ok(source) => {
                self.desk.source.notice = None;
                self.desk.source.edit = Some(Settings {
                    draft: source.clone(),
                    original: source,
                    confirm: false,
                });
            }
            Err(e) => {
                self.refresh();
                self.error = Some(e.to_string());
            }
        }
    }
    pub fn save_source_settings(&mut self) {
        if self.page != Page::Inbox {
            return;
        }
        let Some(edit) = self.desk.source.edit.as_mut() else {
            return;
        };
        let (old, draft) = (&edit.original, &edit.draft);
        if (old.outbound != draft.outbound
            || (old.settings.mirror_files && !draft.settings.mirror_files))
            && !edit.confirm
        {
            edit.confirm = true;
            return;
        }
        let before = old.clone();
        let patch = SourcePatch {
            outbound: (old.outbound != draft.outbound).then_some(draft.outbound),
            mirror_roots: (old.settings.mirror_roots != draft.settings.mirror_roots)
                .then_some(draft.settings.mirror_roots),
            mirror_files: (old.settings.mirror_files != draft.settings.mirror_files)
                .then_some(draft.settings.mirror_files),
            display_authors: (old.settings.display_authors != draft.settings.display_authors)
                .then_some(draft.settings.display_authors),
            github_items: (old.settings.github_items != draft.settings.github_items)
                .then_some(draft.settings.github_items),
        };
        match self
            .db()
            .and_then(|b| b.patch_source_if(&self.actor, &before.name, Some(&before), &patch))
        {
            Ok(_) => {
                self.cancel_source_form();
                self.refresh();
            }
            Err(e) => {
                self.refresh();
                self.desk.source.notice = Some(e.to_string());
            }
        }
    }
    pub fn copy_source_command(&mut self, tx: &mut Tx) {
        let result = self.db().and_then(|b| {
            ensure!(self.page == Page::Inbox, "No selected source");
            let source = b.source(&self.actor, &self.desk.source.selected)?;
            let path =
                quoted_database(b).context("This database does not have a shell-safe file path")?;
            Ok(format!("board --db {path} {}", source_args(&source)))
        });
        match result {
            Ok(cmd) => tx.copy(cmd),
            Err(e) => {
                self.refresh();
                self.error = Some(e.to_string());
            }
        }
    }
}
fn quoted_database(b: &Board) -> Option<String> {
    let path = b.database_file()?.to_str()?;
    // Never paste terminal controls, line separators, or a lossy filename.
    if path
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        return None;
    }
    #[cfg(windows)]
    {
        // PowerShell recognizes smart quotes as delimiters as well as ASCII.
        if path.contains([
            '\u{2018}', '\u{2019}', '\u{201a}', '\u{201b}', '\u{201c}', '\u{201d}', '\u{201e}',
            '\u{201f}',
        ]) {
            return None;
        }
        Some(format!("'{}'", path.replace('\'', "''")))
    }
    #[cfg(not(windows))]
    {
        Some(format!("'{}'", path.replace('\'', "'\\''")))
    }
}
fn source_args(source: &Source) -> String {
    let positional = if source.name.starts_with('-') {
        "-- "
    } else {
        ""
    };
    if source.provider == Provider::Local {
        format!("sync import {positional}{} event.json", source.name)
    } else {
        format!("sync pull {positional}{}", source.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use board_local::access::Rules;
    use rune::{Env, Runtime, clipboard};

    #[test]
    fn copying_a_source_command_uses_a_literal_positional_and_rechecks_current_authorization() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reader's work $board.db");
        let b = Board::open(&path).unwrap();
        b.create_source(
            "reader",
            "-updates",
            Provider::Local,
            "event stream",
            "work",
        )
        .unwrap();
        let grant = b
            .set_grant(
                "owner",
                None,
                "reader",
                &Rules::parse("work=read,sync").unwrap(),
            )
            .unwrap();
        let token = b.issue_credential("owner", "reader", None, None).unwrap();
        let mut client = Board::open(&path).unwrap();
        client.bind_token(&token.token).unwrap();
        let mut state = State::test_board(client, Some("reader"));
        state.desk.source.selected = "-updates".into();
        state.go(Page::Inbox);
        let mut rt = Runtime::new(state, crate::view::view, (1000., 800.), Env::default());
        let clip = clipboard::memory();
        rt.use_clipboard(&clip);
        rt.frame(0.);
        let (x, y) = rt.find("source-copy-command").unwrap();
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.04);
        let clipboard_text = clip.text().unwrap();
        let copied = clipboard_text.as_str();
        assert!(copied.starts_with("board --db "));
        assert!(!copied.contains(&token.token));
        #[cfg(unix)]
        {
            let argv = std::process::Command::new("sh")
                .args([
                    "-c",
                    &format!("board() {{ printf '%s\\0' \"$@\"; }}; {copied}"),
                ])
                .output()
                .unwrap();
            assert!(argv.status.success());
            let args: Vec<_> = argv
                .stdout
                .strip_suffix(&[0])
                .unwrap()
                .split(|b| *b == 0)
                .collect();
            assert_eq!(
                args,
                [
                    "--db",
                    b.database_file().unwrap().to_str().unwrap(),
                    "sync",
                    "import",
                    "--",
                    "-updates",
                    "event.json"
                ]
                .map(str::as_bytes)
            );
            if let Some(cli) = std::env::var_os("BOARD_UI_TEST_CLI") {
                let other = dir.path().join("ambient.sqlite3");
                let ambient = Board::open(&other).unwrap();
                ambient
                    .create_source(
                        "reader",
                        "-updates",
                        Provider::Local,
                        "elsewhere",
                        "wrong-board",
                    )
                    .unwrap();
                let event = serde_json::json!({"key":"one","import":{"thread":"topic","external":"root","author":"writer","change":{"op":"post","text":"only in the selected board"}}});
                std::fs::write(
                    dir.path().join("event.json"),
                    serde_json::to_vec(&event).unwrap(),
                )
                .unwrap();
                let out = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(format!(
                        "board() {{ \"$BOARD_UI_TEST_CLI\" \"$@\"; }}; {copied}"
                    ))
                    .current_dir(dir.path())
                    .env("BOARD_UI_TEST_CLI", std::fs::canonicalize(cli).unwrap())
                    .env("BOARD_DB", &other)
                    .env("BOARD_USER", "reader")
                    .env_remove("BOARD_NAME")
                    .env_remove("BOARD_AVATAR")
                    .env_remove("BOARD_PROFILE")
                    .env_remove("BOARD_TOKEN")
                    .output()
                    .unwrap();
                assert!(
                    out.status.success(),
                    "{}",
                    String::from_utf8_lossy(&out.stderr)
                );
                assert!(
                    b.find_source("reader", "-updates", "topic", None)
                        .unwrap()
                        .is_some()
                );
                assert!(
                    ambient
                        .find_source("reader", "-updates", "topic", None)
                        .unwrap()
                        .is_none()
                );
            }
        }
        #[cfg(windows)]
        assert!(copied.contains("reader''s work $board.db"));
        clip.set_text("unchanged");
        b.set_grant(
            "owner",
            Some(grant.id),
            "reader",
            &Rules::parse("work=read").unwrap(),
        )
        .unwrap();
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.08);
        assert_eq!(clip.text().unwrap().as_str(), "unchanged");
    }

    #[cfg(unix)]
    #[test]
    fn a_non_text_or_control_database_path_has_no_copyable_command() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        for file in [b"non-utf8-\xff.db".as_slice(), b"two\nlines.db"] {
            let path = dir.path().join(std::ffi::OsStr::from_bytes(file));
            let db = Board::open(path).unwrap();
            assert!(quoted_database(&db).is_none());
            db.create_source("user", "feed", Provider::Local, "stream", "work")
                .unwrap();
            let mut state = State::test_board(db, None);
            state.actor = "user".into();
            state.desk.source.selected = "feed".into();
            state.go(Page::Inbox);
            let mut rt = Runtime::new(state, crate::view::view, (1000., 800.), Env::default());
            rt.frame(0.);
            assert!(rt.find("source-configure").is_some());
            assert!(rt.find("source-copy-command").is_none());
        }
        assert!(quoted_database(&Board::memory().unwrap()).is_none());
    }
}
