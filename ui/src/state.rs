mod browse;
mod drafts;
mod files;
mod people;
mod sources;
mod templates;
mod video;
mod workspace;
pub use browse::{Browse, Position};
pub use drafts::Delivery;
pub use files::{AttachmentPreview, Preview};
pub use people::People;
pub use templates::SchemaPart;
pub use video::Clip;
pub use workspace::{ActivityMode, Desk, TaskCommand};

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use board_local::{Board, Channel, Filter, Message, PostOptions, ThreadSummary, automation, local};
use rune::{TextEdit, Tx};

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub enum Page {
    Feed,
    Compose,
    Thread(i64),
    Activity,
    Tasks,
    Task(i64),
    NewTask,
    Workspace,
    Schedules,
    Watchers,
    Schemas,
    Access,
    Sources,
    Inbox,
    Profiles,
    People,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Property {
    Labels,
    Reactions,
    Body,
}

pub struct Properties {
    pub id: i64,
    pub revision: i64,
    pub property: Property,
    pub edit: TextEdit,
    pub label_key: TextEdit,
    pub label_value: TextEdit,
    pub reaction: TextEdit,
    pub confirm_delete: bool,
    pub deliveries: Vec<board_local::sources::DeliveryNotice>,
    labels_at_open: BTreeMap<String, String>,
}

impl Properties {
    fn new(message: &Message) -> Self {
        Self {
            id: message.id,
            revision: message.revision,
            property: Property::Labels,
            edit: TextEdit::new(&message.text),
            label_key: TextEdit::new(""),
            label_value: TextEdit::new(""),
            reaction: TextEdit::new(""),
            confirm_delete: false,
            deliveries: Vec::new(),
            labels_at_open: message.labels.clone(),
        }
    }

    pub fn select_label(&mut self, key: &str, value: &str) {
        self.label_key = TextEdit::new(key);
        self.label_value = TextEdit::new(value);
        self.labels_at_open.insert(key.to_owned(), value.to_owned());
    }
}

type Files = Vec<(String, Vec<u8>)>;

pub struct State {
    board: Option<Board>,
    pub actor: String,
    pub delegated: bool,
    pub access_blocked: bool,
    pub desk: Desk,
    pub history: Vec<Page>,
    pub page: Page,
    pub channel: Option<String>,
    pub channels: Vec<Channel>,
    pub threads: Vec<ThreadSummary>,
    pub thread: Vec<Message>,
    pub browse: Browse,
    pub people: People,
    pub typing: Vec<board_local::presence::Typing>,
    typing_attempt: Option<(i64, Instant)>,
    pub query: TextEdit,
    pub tasks_only: bool,
    pub destination: TextEdit,
    pub body: TextEdit,
    pub files: Files,
    pub reply_to: Option<i64>,
    pub properties: Option<Properties>,
    pub preview: Option<AttachmentPreview>,
    pub raw_blocks: HashSet<(i64, usize)>,
    pub delivery: Delivery,
    draft_dirty: bool,
    pub error: Option<String>,
    pub starts: Vec<i64>,
    pub older: Option<i64>,
    pub nav: bool,
    version: i64,
    next_poll: f64,
}

impl State {
    fn connect() -> Result<(Board, Option<String>)> {
        let mut db = Board::open(local::database_path())?;
        let actor = db.bind_env()?;
        Ok((db, actor))
    }

    pub fn open() -> Self {
        Self::opened(Self::connect())
    }

    fn opened(connection: Result<(Board, Option<String>)>) -> Self {
        let (board, identity, error) = match connection {
            Ok((b, actor)) => (Some(b), actor, None),
            Err(e) => (None, None, Some(format!("{e:#}"))),
        };
        let mut s = Self {
            board,
            error,
            delegated: identity.is_some(),
            access_blocked: false,
            actor: identity.unwrap_or_else(local::actor_name),
            desk: Desk::default(),
            history: Vec::new(),
            page: Page::Feed,
            channel: None,
            channels: Vec::new(),
            threads: Vec::new(),
            thread: Vec::new(),
            browse: Browse::default(),
            people: People::default(),
            typing: Vec::new(),
            typing_attempt: None,
            query: TextEdit::new(""),
            tasks_only: false,
            destination: TextEdit::new("general"),
            body: TextEdit::new(""),
            files: Vec::new(),
            reply_to: None,
            properties: None,
            preview: None,
            raw_blocks: HashSet::new(),
            delivery: Delivery::default(),
            draft_dirty: false,
            starts: Vec::new(),
            older: None,
            nav: false,
            version: -1,
            next_poll: 0.0,
        };
        if s.board.is_some() {
            s.refresh();
        }
        s
    }

    fn db(&self) -> Result<&Board> {
        self.board.as_ref().context("Database is not open")
    }

    pub fn connected(&self) -> bool {
        self.board.is_some()
    }

    pub fn retry(&mut self) {
        if self.board.is_none() {
            match Self::connect() {
                Ok((board, identity)) => {
                    self.delegated = identity.is_some();
                    self.actor = identity.unwrap_or_else(local::actor_name);
                    self.board = Some(board);
                }
                Err(e) => {
                    self.error = Some(format!("{e:#}"));
                    return;
                }
            }
        }
        self.error = None;
        self.refresh();
    }

    fn load(&mut self) -> Result<()> {
        let db = self.board.as_ref().context("Database is not open")?;
        // Sample before reading: a concurrent writer during the load forces another refresh.
        let version = db.data_version()?;
        let channels = db.channels()?;
        let mut filter = self.browse.filter.clone();
        filter.channel = if self.page == Page::Feed {
            self.channel.clone()
        } else {
            None
        };
        filter.text =
            (!self.query.text.trim().is_empty()).then(|| self.query.text.trim().to_owned());
        if self.tasks_only && self.page == Page::Feed {
            filter.labels.insert("board.task".into(), String::new());
        }
        let (threads, older) = if self.page == Page::Feed {
            let mut window = db.threads_window(&filter, self.starts.last().copied(), 50)?;
            window.rows.reverse();
            (window.rows, window.older)
        } else {
            (Vec::new(), None)
        };
        let thread = match self.page {
            Page::Thread(id) => self.browse.read_thread(db, id)?,
            _ => Vec::new(),
        };
        let typing = match self.page {
            Page::Thread(id) => db
                .typing(id)?
                .into_iter()
                .filter(|t| t.actor != self.actor)
                .collect(),
            _ => Vec::new(),
        };
        self.desk
            .load(db, &self.actor, self.page, self.delegated, &filter)?;
        if self.page == Page::People || self.people.picking {
            self.people.rows = db.actor_directory(Some(self.people.query.text.trim()))?;
            self.people.offset = self
                .people
                .offset
                .min(self.people.rows.len().saturating_sub(1) / 30 * 30);
        }
        self.version = version;
        self.channels = channels;
        self.threads = threads;
        self.older = older;
        self.thread = thread;
        if let Some(p) = &mut self.properties {
            p.deliveries = db.message_deliveries(&self.actor, p.id)?;
        }
        if self.preview.as_ref().is_some_and(|p| {
            !self.thread.iter().any(|m| {
                m.id == p.message && !m.deleted && m.attachments.iter().any(|a| a.id == p.file.id)
            })
        }) {
            self.preview = None;
        }
        self.typing = typing;
        Ok(())
    }

    pub fn refresh(&mut self) {
        if let Err(e) = self.load() {
            self.access_blocked = self.delegated;
            self.channels.clear();
            self.threads.clear();
            self.thread.clear();
            self.browse.schedules.clear();
            self.preview = None;
            self.typing.clear();
            self.people.rows.clear();
            self.desk.clear_data();
            self.error = Some(format!("{e:#}"));
        } else if self.access_blocked {
            self.access_blocked = false;
            self.error = None;
        }
    }

    pub fn select(&mut self, channel: Option<String>) {
        self.channel = channel;
        self.query.clear();
        self.starts.clear();
        self.go(Page::Feed);
    }

    pub fn go(&mut self, page: Page) {
        let mut draft_warning = None;
        self.properties = None;
        self.preview = None;
        self.browse.jump_open = false;
        self.browse.filter_open = false;
        self.people = People::default();
        if self.page != page {
            if let Err(e) = self.flush_draft() {
                self.error = Some(format!("Draft not saved: {e:#}"));
                self.delivery.open = true;
                return;
            }
            self.clear_typing();
            self.history.push(self.page);
            if self.history.len() > 64 {
                self.history.remove(0);
            }
            self.raw_blocks.clear();
            self.body.clear();
            self.reply_to = None;
            self.files.clear();
            self.delivery = Delivery::default();
            self.destination = TextEdit::new(
                self.channel
                    .as_deref()
                    .unwrap_or("general")
                    .trim_end_matches('/'),
            );
            self.desk.enter(page);
            self.browse.reset_thread();
            self.page = page;
            if let Err(e) = self.restore_draft() {
                self.body.clear();
                self.reply_to = None;
                self.files.clear();
                draft_warning = Some(format!("Draft not available: {e:#}"));
            }
        }
        self.nav = false;
        self.error = draft_warning;
        self.refresh();
    }

    pub fn back(&mut self) {
        let page = self.history.pop().unwrap_or(Page::Feed);
        let history = std::mem::take(&mut self.history);
        self.go(page);
        self.history = history;
    }

    pub fn star(&mut self, id: i64, remove: bool) {
        match self
            .db()
            .and_then(|db| db.react(id, &self.actor, "star", remove))
        {
            Ok(_) => {
                self.error = None;
                self.refresh();
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    pub fn note_typing(&mut self) {
        let Page::Thread(id) = self.page else { return };
        if self.body.text.trim().is_empty() {
            self.clear_typing();
            return;
        }
        if self
            .typing_attempt
            .is_some_and(|(root, t)| root == id && t.elapsed() < Duration::from_secs(3))
        {
            return;
        }
        self.typing_attempt = Some((id, Instant::now()));
        if let Some(db) = self.board.as_ref() {
            // Presence is advisory. A read-only session can still draft a reply; sending
            // it is the place to report the authoritative permission error.
            let _ = db.set_typing(id, &self.actor, Some("typing"), 10);
        }
    }

    fn clear_typing(&mut self) {
        if let Some((id, _)) = self.typing_attempt.take()
            && let Some(db) = self.board.as_ref()
        {
            let _ = db.set_typing(id, &self.actor, None, 10);
        }
    }

    pub fn open_properties(&mut self, id: i64) {
        if let Some(m) = self.thread.iter().find(|m| m.id == id) {
            self.properties = Some(Properties::new(m));
            self.error = None;
            self.refresh();
        }
    }

    pub fn copy_message_text(&mut self, id: i64, tx: &mut Tx) {
        match self.db().and_then(|b| b.get(id)) {
            Ok(m) if !m.deleted => tx.copy(m.text),
            Ok(_) => self.error = Some("Message deleted".into()),
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    pub fn close_properties(&mut self, tx: &mut Tx) {
        if let Some(p) = self.properties.take() {
            let key = format!("inspect-{}", p.id);
            tx.focus_when_shown(&key, true);
            tx.scroll_into_view(key);
        }
        self.error = None;
    }

    // Polling may refresh the thread while this editor is open. Only an explicit
    // reload or a successful mutation changes what the next conditional write expects.
    fn property_write(
        &mut self,
        write: impl FnOnce(&Board, &str, &Properties) -> Result<Message>,
    ) -> Option<Message> {
        let p = self.properties.as_ref()?;
        match self.db().and_then(|b| write(b, &self.actor, p)) {
            Ok(m) => {
                self.error = None;
                self.refresh();
                Some(m)
            }
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                None
            }
        }
    }

    pub fn change_label(&mut self, remove: bool) {
        let changed = self.property_write(|b, actor, p| {
            let k = p.label_key.text.trim().to_owned();
            let mut set = BTreeMap::new();
            let mut unset = Vec::new();
            if remove {
                unset.push(k.clone());
            } else {
                set.insert(k.clone(), p.label_value.text.clone());
            }
            b.labels(
                p.id,
                actor,
                &set,
                &unset,
                &[(k.clone(), p.labels_at_open.get(&k).cloned())].into(),
            )
        });
        if let (Some(m), Some(p)) = (changed, &mut self.properties) {
            let k = p.label_key.text.trim();
            if let Some(value) = m.labels.get(k) {
                p.labels_at_open.insert(k.to_owned(), value.clone());
            } else {
                p.labels_at_open.remove(k);
            }
        }
    }

    pub fn change_reaction(&mut self, remove: bool) {
        self.property_write(|b, actor, p| b.react(p.id, actor, p.reaction.text.trim(), remove));
    }

    pub fn change_body(&mut self, delete: bool, tx: &mut Tx) {
        let changed = self.property_write(|b, actor, p| {
            if delete {
                anyhow::ensure!(p.confirm_delete, "Deletion was not confirmed");
                b.delete(p.id, actor, Some(p.revision))
            } else {
                b.edit(p.id, actor, &p.edit.text, Some(p.revision))
            }
        });
        if changed.is_some() {
            self.close_properties(tx);
        }
    }

    pub fn poll(&mut self, now: f64, tx: &mut Tx) -> bool {
        if let Some(id) = self.browse.reveal.take() {
            let key = format!("inspect-{id}");
            tx.focus_when_shown(&key, true);
            tx.scroll_into_view(key);
        }
        let frame_changed = if let Some(AttachmentPreview {
            content: Preview::Clip(clip),
            ..
        }) = &mut self.preview
        {
            clip.poll(now, tx)
        } else {
            false
        };
        if now < self.next_poll {
            return frame_changed;
        }
        self.next_poll = now + 1.0;
        tx.wake_after(Duration::from_millis(1050));
        let mut draft_update = false;
        if self.draft_dirty {
            match self.flush_draft() {
                Ok(()) => {
                    if self
                        .error
                        .as_ref()
                        .is_some_and(|e| e.starts_with("Draft not saved:"))
                    {
                        self.error = None;
                        draft_update = true;
                    }
                }
                Err(e) => {
                    if self.error.is_none() {
                        self.error = Some(format!("Draft not saved: {e:#}"));
                        draft_update = true;
                    }
                }
            }
        }
        let Some(db) = self.board.as_ref() else {
            return false;
        };
        // A credential can expire by wall time without changing SQLite's data_version.
        if self.delegated && db.activity_seen(&self.actor).is_err() {
            self.refresh();
            return true;
        }
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let length = self.typing.len();
        self.typing.retain(|t| t.expires > wall);
        if let Some(task) = self
            .thread
            .first()
            .and_then(|m| m.task.as_ref())
            .or_else(|| {
                self.desk
                    .tasks
                    .iter()
                    .find(|t| t.lease_until.is_some_and(|until| until <= wall))
            })
            && task.lease_until.is_some_and(|until| until <= wall)
        {
            match automation::get_task(db, task.id) {
                Ok(_) => self.refresh(),
                Err(e) => self.error = Some(format!("{e:#}")),
            }
            return true;
        }
        match db.data_version() {
            Ok(version) if version == self.version => {
                frame_changed || draft_update || length != self.typing.len()
            }
            Ok(_) => {
                self.refresh();
                true
            }
            Err(e) => {
                self.error = Some(e.to_string());
                true
            }
        }
    }
}
