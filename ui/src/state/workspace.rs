use super::*;
use anyhow::{bail, ensure};
use automation::{Task, Watcher};
use board_local::{Event, Schema};

const PAGE: usize = 40;
const LEASE: i64 = 300;

#[derive(Default, Clone, Copy, Eq, PartialEq)]
pub enum ActivityMode {
    #[default]
    Messages,
    Changes,
    Unread,
}

#[derive(Clone, Copy)]
pub enum TaskCommand {
    Claim,
    Renew,
    Release,
    Complete,
    Fail,
    Block,
    Resume,
    Cancel,
    Assign,
    Needs,
}

pub struct Form {
    pub name: TextEdit,
    pub channel: TextEdit,
    pub body: TextEdit,
    pub value: TextEdit,
    pub extra: TextEdit,
}
pub struct TaskEntry {
    instance: std::sync::Arc<()>,
    pub form: Form,
    pub labels: TextEdit,
    pub files: Vec<(String, Vec<u8>)>,
    pub no_sync: bool,
    pub show_labels: bool,
    pub reload_needed: bool,
    pub(super) stamp: Option<String>,
    pub(super) baseline: Option<(board_local::Draft, board_local::ComposerSettings)>,
    pub files_changed: bool,
}
impl Default for TaskEntry {
    fn default() -> Self {
        Self {
            instance: Default::default(),
            form: Form::default(),
            labels: TextEdit::new(""),
            files: Vec::new(),
            no_sync: false,
            show_labels: false,
            reload_needed: false,
            stamp: None,
            baseline: None,
            files_changed: false,
        }
    }
}

#[derive(Default)]
pub struct AccessReview {
    pub viewer: Option<board_local::access::Viewer>,
    pub fields: Form,
    pub other: bool,
    pub query: Option<(String, String, bool)>,
    pub decision: Option<board_local::access::Decision>,
}
impl AccessReview {
    pub fn registry(&self) -> bool {
        self.viewer.as_ref().is_some_and(|v| {
            v.owner
                || v.can
                    .0
                    .get("*")
                    .is_some_and(|a| a.contains(&board_local::access::Action::Manage))
        })
    }
    fn decision(
        db: &Board,
        v: &board_local::access::Viewer,
        subject: &str,
        channel: &str,
        other: bool,
    ) -> Result<board_local::access::Decision> {
        if v.owner && !other {
            // A local owner is not constrained by grants a token for the same name would receive.
            board_local::validate_channel(channel)?;
            return Ok(board_local::access::Decision {
                actions: v.can.0.get("*").into_iter().flatten().copied().collect(),
                pattern: Some("*".into()),
                level: Some("local owner".into()),
                over: vec![],
                grants: vec![],
            });
        }
        db.explain_decision(subject, channel)
    }
}
impl Default for Form {
    fn default() -> Self {
        Self {
            name: TextEdit::new(""),
            channel: TextEdit::new(""),
            body: TextEdit::new(""),
            value: TextEdit::new(""),
            extra: TextEdit::new(""),
        }
    }
}

#[derive(Default)]
pub struct Desk {
    pub attention: Vec<board_local::swarm::AttentionItem>,
    pub selected_decision: Option<i64>,
    pub decision_answer: TextEdit,
    pub mode: ActivityMode,
    pub messages: Vec<Message>,
    pub events: Vec<Event>,
    pub seen: i64,
    pub unread: i64,
    pub head: i64,
    pub through: i64,
    pub tasks: Vec<Task>,
    pub selected_task: Option<Task>,
    pub task_options: bool,
    pub task_rows: HashMap<i64, TaskRow>,
    pub task_filter: usize,
    pub filter_form: Form,
    pub filtering: bool,
    pub task_entry: TaskEntry,
    pub access: AccessReview,
    pub criteria: automation::TaskFilter,
    pub watchers: Vec<Watcher>,
    pub schemas: Vec<Schema>,
    pub grants: Vec<board_local::access::Grant>,
    pub credentials: Vec<board_local::access::Credential>,
    pub source: super::sources::Review,
    pub swarm: super::swarms::Observer,
    pub roles: super::roles::Library,
    pub profiles: Vec<String>,
    pub schema_open: Option<super::templates::SchemaPage>,
    pub confirm: Option<String>,
    pub editing: bool,
    pub form: Form,
    pub pages: Vec<i64>,
    pub next: Option<i64>,
    pub watcher_pages: Vec<String>,
    pub watcher_next: Option<String>,
    claims: HashMap<(String, i64), (i64, String)>,
}

pub struct TaskRow {
    pub created: String,
    pub labels: BTreeMap<String, String>,
    pub files: String,
}
impl Desk {
    pub const TASK_STATES: [&'static str; 8] = [
        "all",
        "waiting",
        "ready",
        "claimed",
        "blocked",
        "completed",
        "failed",
        "canceled",
    ];
    pub fn clear_data(&mut self) {
        self.attention.clear();
        self.selected_decision = None;
        self.decision_answer.clear();
        self.task_entry.instance = Default::default();
        self.messages.clear();
        self.events.clear();
        self.tasks.clear();
        self.selected_task = None;
        self.task_rows.clear();
        self.access.viewer = None;
        self.access.decision = None;
        self.watchers.clear();
        self.schemas.clear();
        self.grants.clear();
        self.credentials.clear();
        self.source.clear_data();
        self.swarm = Default::default();
        self.roles = Default::default();
        self.profiles.clear();
        self.unread = 0;
        self.next = None;
        self.watcher_next = None;
        self.seen = 0;
        self.through = 0;
        self.head = 0;
    }

    pub fn first_page(&mut self) {
        self.pages.clear();
        self.watcher_pages.clear();
    }
    pub fn enter_from(&mut self, old: Page, page: Page) {
        let keep = matches!(old, Page::Tasks | Page::Task(_))
            && matches!(page, Page::Tasks | Page::Task(_));
        let pages = if keep {
            std::mem::take(&mut self.pages)
        } else {
            Vec::new()
        };
        self.enter(page);
        self.pages = pages;
    }
    pub fn enter(&mut self, page: Page) {
        self.first_page();
        self.clear_data();
        self.form = Form::default();
        self.access = AccessReview::default();
        self.editing = false;
        self.task_options = false;
        self.filtering = false;
        self.filter_form = Form::default();
        self.confirm = None;
        self.schema_open = None;
        self.source.expanded = None;
        if page == Page::NewTask && self.task_entry.form.channel.text.is_empty() {
            self.task_entry.form.channel = TextEdit::new("general");
        }
    }

    pub fn load(
        &mut self,
        db: &Board,
        actor: &str,
        page: Page,
        delegated: bool,
        filter: &Filter,
    ) -> Result<()> {
        self.attention = db.attention(actor)?;
        self.head = db.cursor()?;
        self.seen = db.activity_seen(actor)?;
        self.unread = db.activity_unread(actor)?;
        self.next = None;
        self.watcher_next = None;
        let after = self.pages.last().copied().unwrap_or(0);
        match page {
            Page::Attention => (),
            Page::Activity => match self.mode {
                ActivityMode::Messages => {
                    let window = db.messages_window(filter, self.pages.last().copied(), PAGE)?;
                    self.next = window.older;
                    self.through = window.snapshot;
                    self.messages = window.rows;
                }
                ActivityMode::Changes | ActivityMode::Unread => {
                    let unread = self.mode == ActivityMode::Unread;
                    let latest = !unread && self.pages.is_empty();
                    let start = if unread { after.max(self.seen) } else { after };
                    let (cursor, mut events) = if latest {
                        db.event_tail(PAGE, &Filter::default())?
                    } else if unread {
                        let mut found = Vec::new();
                        let mut scan = start;
                        while scan < self.head && found.len() <= PAGE {
                            let (next, batch) =
                                db.event_page(scan, 128, &Filter::default(), None)?;
                            found.extend(
                                batch
                                    .into_iter()
                                    .take_while(|e| e.cursor <= self.head)
                                    .filter(|e| e.actor != actor)
                                    .take(PAGE + 1 - found.len()),
                            );
                            if next <= scan {
                                break;
                            }
                            scan = next.min(self.head);
                        }
                        (scan, found)
                    } else {
                        db.event_page(start, PAGE + 1, &Filter::default(), None)?
                    };
                    if events.len() > PAGE {
                        events.truncate(PAGE);
                        self.next = events.last().map(|e| e.cursor);
                    }
                    self.through = self.next.unwrap_or(cursor);
                    if !latest {
                        // Events keep their original identity and cursor. A visual preview
                        // must not put superseded or deleted message text back on screen.
                        let mut previews: HashMap<i64, Message> = HashMap::new();
                        for e in &mut events {
                            if let Some(m) = &mut e.message {
                                let current = match previews.entry(m.id) {
                                    std::collections::hash_map::Entry::Vacant(v) => {
                                        v.insert(db.get(m.id)?)
                                    }
                                    std::collections::hash_map::Entry::Occupied(v) => v.into_mut(),
                                };
                                m.clone_from(current);
                            }
                        }
                    }
                    self.events = events;
                }
            },
            Page::Tasks | Page::Task(_) => {
                let state = Self::TASK_STATES[self.task_filter];
                self.criteria.state = Some(state.into());
                let (_, mut tasks) =
                    automation::filtered_tasks_page(db, &self.criteria, after, PAGE + 1)?;
                if tasks.len() > PAGE {
                    tasks.truncate(PAGE);
                    self.next = tasks.last().map(|t| t.id);
                }
                self.task_rows = tasks
                    .iter()
                    .map(|t| {
                        let m = db.get(t.id)?;
                        Ok((
                            t.id,
                            TaskRow {
                                created: m.created_at,
                                labels: m.labels,
                                files: m
                                    .attachments
                                    .into_iter()
                                    .map(|f| f.name)
                                    .collect::<Vec<_>>()
                                    .join(", "),
                            },
                        ))
                    })
                    .collect::<Result<_>>()?;
                self.tasks = tasks;
                if let Page::Task(id) = page {
                    let t = automation::get_task(db, id)?;
                    if self.selected_task.as_ref().is_none_or(|old| old.id != id) {
                        self.form.name = TextEdit::new(t.assignee.as_deref().unwrap_or(""));
                        self.form.extra = TextEdit::new(
                            t.needs
                                .iter()
                                .map(i64::to_string)
                                .collect::<Vec<_>>()
                                .join(", "),
                        );
                    }
                    self.selected_task = Some(t);
                } else {
                    self.selected_task = None;
                }
            }
            Page::Watchers => {
                let (_, mut rows) = automation::watchers_page(
                    db,
                    self.watcher_pages.last().map_or("", String::as_str),
                    PAGE + 1,
                )?;
                if rows.len() > PAGE {
                    rows.truncate(PAGE);
                    self.watcher_next = rows.last().map(|w| w.name.clone());
                }
                self.watchers = rows;
            }
            Page::Schemas => self.schemas = db.schemas()?,
            Page::Access | Page::Workspace => {
                self.access.viewer = Some(db.viewer(actor)?);
                let registry = self.access.registry();
                self.grants = if registry { db.grants()? } else { vec![] };
                self.credentials = if delegated { vec![] } else { db.credentials()? };
                if self
                    .access
                    .query
                    .as_ref()
                    .is_some_and(|(_, _, other)| *other && !registry)
                {
                    self.access.query = None;
                    self.access.other = false;
                    self.access.fields.name.clear();
                }
                self.access.decision = self
                    .access
                    .query
                    .as_ref()
                    .map(|(who, ch, other)| {
                        AccessReview::decision(
                            db,
                            self.access.viewer.as_ref().unwrap(),
                            who,
                            ch,
                            *other,
                        )
                    })
                    .transpose()?;
            }
            Page::Sources => self.source.listing(db, actor)?,
            Page::Swarms | Page::Swarm(_) => {
                self.swarm.load(db, actor, page)?;
            }
            Page::Roles if !delegated => self.roles.load(db)?,
            Page::Inbox => self.next = self.source.load(db, actor, after, PAGE)?,
            Page::Profiles => {
                let person = local::person_name();
                self.profiles = if delegated {
                    vec![actor.to_owned()]
                } else {
                    let mut names: Vec<_> = db
                        .actors()?
                        .into_iter()
                        .filter(|n| board_local::owned_by(n, &person))
                        .collect();
                    names.extend([person, actor.to_owned()]);
                    names.sort();
                    names.dedup();
                    names
                };
            }
            _ => (),
        }
        Ok(())
    }
    pub fn has_claim(&self, actor: &str, task: &Task) -> bool {
        task.holder.as_deref() == Some(actor)
            && self
                .claims
                .get(&(actor.to_owned(), task.id))
                .is_some_and(|(epoch, _)| *epoch == task.epoch)
    }
}

fn ids(text: &str) -> Result<Vec<i64>> {
    board_local::TaskDraftSettings {
        needs: text.to_owned(),
        ..Default::default()
    }
    .parsed_needs()
}

impl State {
    pub fn explain_access(&mut self) {
        let review = &self.desk.access;
        let other = review.other && review.registry();
        let who = if other {
            review.fields.name.text.trim()
        } else {
            &self.actor
        }
        .to_owned();
        let ch = review
            .fields
            .channel
            .text
            .trim()
            .trim_start_matches('#')
            .to_owned();
        let result = self.db().and_then(|db| {
            AccessReview::decision(
                db,
                review.viewer.as_ref().context("No current permissions")?,
                &who,
                &ch,
                other,
            )
        });
        if let Some(decision) = self.result(result) {
            self.desk.access.query = Some((who, ch, other));
            self.desk.access.decision = Some(decision);
        }
    }
    pub fn open_message_thread(&mut self, id: i64) {
        let result = self.db().and_then(|b| b.get(id));
        if let Some(m) = self.result(result) {
            let root = m.re.unwrap_or(id);
            let page = if self.page == Page::Task(root) {
                Page::Task(root)
            } else {
                Page::Thread(root)
            };
            self.go(page);
            if self.page != page {
                return;
            }
            self.set_task_options(false);
            self.browse.position = if m.re.is_some() {
                Position::Before(id.saturating_add(1))
            } else {
                Position::Latest
            };
            self.browse.focused = Some(id);
            self.browse.reveal = Some(id);
            self.refresh();
        }
    }
    pub(super) fn result<T>(&mut self, result: Result<T>) -> Option<T> {
        match result {
            Ok(value) => {
                self.error = None;
                Some(value)
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                None
            }
        }
    }

    pub fn mark_seen(&mut self, through: i64) {
        let result = self
            .db()
            .and_then(|db| db.mark_activity_seen(&self.actor, through));
        if self.result(result).is_some() {
            self.desk.first_page();
            self.refresh();
        }
    }

    pub fn switch_profile(&mut self, actor: &str) {
        let person = local::person_name();
        let profile = actor.strip_prefix(&format!("{person}/"));
        if self.delegated
            || !(actor == person
                || profile.is_some_and(|p| {
                    p.len() <= 32
                        && p.bytes().next().is_some_and(|c| c.is_ascii_lowercase())
                        && p.bytes()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
                }))
        {
            self.error=Some("Use a local profile name starting with a lowercase letter, up to 32 letters, digits or hyphens.".into());
            return;
        }
        // Entering Profiles already parked any active draft under the old actor.
        if actor != self.actor
            && let Some(db) = self.board.as_mut()
        {
            db.clear_write_avatar();
        }
        self.actor = actor.to_owned();
        self.desk.source.selected.clear();
        self.desk.source.outgoing = false;
        self.desk.criteria = Default::default();
        self.desk.task_filter = 0;
        self.desk.task_entry = TaskEntry::default();
        self.history.clear();
        self.query.clear();
        self.browse = Browse::default();
        self.reset_feed();
        self.channel = None;
        self.go(Page::Feed);
        self.history.clear();
    }

    pub fn create_task(&mut self, existing: Option<i64>) {
        if existing.is_none()
            && let Err(e) = self.flush_task_entry()
        {
            self.error = Some(format!("Task draft not saved: {e:#}"));
            return;
        }
        let entry = &self.desk.task_entry;
        let form = &entry.form;
        let result = self.db().and_then(|db| {
            let options = if existing.is_none() {
                PostOptions {
                    no_sync: entry.no_sync,
                    files: entry.files.clone(),
                    labels: board_local::ComposerSettings {
                        labels: entry.labels.text.clone(),
                        ..Default::default()
                    }
                    .parsed_labels()?,
                    ..Default::default()
                }
            } else {
                PostOptions::default()
            };
            let target = if let Some(id) = existing {
                automation::TaskTarget::Existing(id)
            } else {
                automation::TaskTarget::New {
                    channel: form.channel.text.trim().trim_start_matches('#'),
                    text: &form.body.text,
                    options: &options,
                }
            };
            let needs = if existing.is_some() {
                Vec::new()
            } else {
                ids(&form.extra.text)?
            };
            let assignee = form.name.text.trim();
            if existing.is_some() {
                automation::create_task(db, &self.actor, target, &needs, None)
            } else {
                automation::create_task_draft(
                    db,
                    &self.actor,
                    target,
                    &needs,
                    (!assignee.is_empty()).then_some(assignee),
                    entry.stamp.as_deref().context("Nothing to send")?,
                )
            }
        });
        if let Err(ref error) = result
            && board_local::errors::classify(error).1 == "draft_changed"
        {
            self.desk.task_entry.reload_needed = true;
        }
        if let Some(t) = self.result(result) {
            if existing.is_none() {
                self.desk.task_entry = TaskEntry::default();
            }
            if existing.is_none() && matches!(self.page, Page::Tasks | Page::Task(_)) {
                self.desk.first_page();
                self.refresh();
            } else {
                self.go(Page::Task(t.id));
            }
        }
    }

    pub fn pick_task_file(&mut self, tx: &mut Tx) {
        let actor = self.actor.clone();
        let page = self.page;
        let entry = &self.desk.task_entry;
        let generation = std::sync::Arc::downgrade(&entry.instance);
        let channel = entry.form.channel.text.clone();
        let private = entry.no_sync;
        let labels = entry.labels.text.clone();
        let assignee = entry.form.name.text.clone();
        let needs = entry.form.extra.text.clone();
        tx.open_file(
            rune::FileDialogOptions {
                title: "Attach task file".into(),
                ..Default::default()
            },
            move |s: &mut State, result, _| match result {
                Ok(Some(path))
                    if generation.upgrade().is_some()
                        && s.actor == actor
                        && s.page == page
                        && !s.access_blocked
                        && !s.desk.filtering
                        && s.desk.task_entry.form.channel.text == channel
                        && s.desk.task_entry.no_sync == private
                        && s.desk.task_entry.labels.text == labels
                        && s.desk.task_entry.form.name.text == assignee
                        && s.desk.task_entry.form.extra.text == needs =>
                {
                    let read = || -> Result<_> {
                        ensure!(
                            s.desk.task_entry.files.len() < board_local::MAX_ATTACHMENTS,
                            "At most {} attachments",
                            board_local::MAX_ATTACHMENTS
                        );
                        if s.delegated {
                            let channel = channel.trim().trim_start_matches('#');
                            ensure!(
                                !channel.is_empty(),
                                "Choose a task channel before attaching a file"
                            );
                            let rights = s.db()?.explain(&actor, channel)?;
                            ensure!(
                                rights.contains(&board_local::access::Action::Post),
                                "Posting to this channel is no longer permitted"
                            );
                            ensure!(
                                labels.trim().is_empty()
                                    || rights.contains(&board_local::access::Action::Label),
                                "Labeling in this channel is no longer permitted"
                            );
                        }
                        super::files::read_attachment(&path)
                    };
                    let outcome = read();
                    if let Some(file) = s.result(outcome) {
                        s.desk.task_entry.files.push(file);
                        s.desk.task_entry.files_changed = true;
                    }
                }
                Ok(Some(_)) => s.error = Some("Task entry changed; file was not attached".into()),
                Err(e) => s.error = Some(e.to_string()),
                Ok(None) => (),
            },
        );
    }

    pub fn edit_task_filters(&mut self) {
        let c = &self.desk.criteria;
        let f = &mut self.desk.filter_form;
        f.channel = TextEdit::new(c.channel.as_deref().unwrap_or(""));
        f.name = TextEdit::new(c.owner.as_deref().unwrap_or(""));
        f.value = TextEdit::new(c.assignee.as_deref().unwrap_or(""));
        f.extra = TextEdit::new(c.needs.map(|n| n.to_string()).unwrap_or_default());
        f.body = TextEdit::new(
            c.label_terms
                .iter()
                .map(|t| {
                    t.values
                        .as_ref()
                        .map_or_else(|| t.key.clone(), |v| format!("{}={}", t.key, v.join("|")))
                })
                .collect::<Vec<_>>()
                .join("\n"),
        );
        self.desk.filtering = true;
    }

    pub fn save_task_filters(&mut self) {
        let build = || -> Result<automation::TaskFilter> {
            let f = &self.desk.filter_form;
            let opt = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_string());
            let terms: Vec<_> = f.body.text.lines().filter_map(opt).collect();
            let c = automation::TaskFilter {
                channel: opt(&f.channel.text),
                owner: opt(&f.name.text),
                assignee: opt(&f.value.text),
                needs: opt(&f.extra.text)
                    .map(|s| s.trim_start_matches('#').parse::<i64>())
                    .transpose()
                    .context("Enter a positive prerequisite ID")?,
                label_terms: board_local::LabelTerm::parse(&terms)?,
                ..Default::default()
            };
            automation::filtered_tasks_page(self.db()?, &c, 0, 1)?;
            Ok(c)
        };
        let result = build();
        if let Some(criteria) = self.result(result) {
            self.desk.criteria = criteria;
            self.desk.filtering = false;
            self.desk.first_page();
            self.refresh();
        }
    }

    pub fn task_command(&mut self, id: i64, cmd: TaskCommand) {
        use automation::TaskAction as A;
        let form = &self.desk.form;
        let key = (self.actor.clone(), id);
        let secret = self.desk.claims.get(&key);
        let result = self.db().and_then(|db| {
            if matches!(cmd, TaskCommand::Claim) {
                return automation::claim(
                    db,
                    &self.actor,
                    automation::ClaimTarget::Id(id),
                    LEASE,
                    &BTreeMap::new(),
                );
            }
            let reason = form.body.text.trim();
            let needs;
            let action = match cmd {
                TaskCommand::Renew => A::Renew(LEASE),
                TaskCommand::Release => A::Release,
                TaskCommand::Complete => A::Complete((!reason.is_empty()).then_some(reason)),
                TaskCommand::Fail => A::Fail(reason),
                TaskCommand::Block => A::Block(reason),
                TaskCommand::Resume => A::Resume,
                TaskCommand::Cancel => A::Cancel(reason),
                TaskCommand::Assign => {
                    A::Assign((!form.name.text.trim().is_empty()).then_some(form.name.text.trim()))
                }
                TaskCommand::Needs => {
                    needs = ids(&form.extra.text)?;
                    A::Needs(&needs)
                }
                TaskCommand::Claim => unreachable!(),
            };
            let held = matches!(
                cmd,
                TaskCommand::Complete
                    | TaskCommand::Fail
                    | TaskCommand::Release
                    | TaskCommand::Renew
            ) || matches!(cmd, TaskCommand::Block)
                && self
                    .desk
                    .selected_task
                    .as_ref()
                    .is_some_and(|t| t.state == "claimed");
            automation::change_task(
                db,
                id,
                &self.actor,
                secret.filter(|_| held).map(|v| v.0),
                secret.filter(|_| held).map(|v| v.1.as_str()),
                action,
            )
        });
        if let Some(t) = self.result(result) {
            if let Some(token) = t.claim_token {
                self.desk.claims.insert(key, (t.epoch, token));
            } else if t.state != "claimed" {
                self.desk.claims.remove(&key);
            }
            self.desk.form.body.clear();
            self.desk.confirm = None;
            self.refresh();
        }
    }

    pub fn save_admin(&mut self) {
        let f = &self.desk.form;
        let actor = &self.actor;
        let result = self.db().and_then(|b| match self.page {
            Page::Watchers => {
                let channel = f.channel.text.trim();
                let kind = f.value.text.trim();
                automation::add_watcher_as(
                    b,
                    actor,
                    f.name.text.trim(),
                    Filter {
                        channel: (!channel.is_empty()).then(|| channel.into()),
                        ..Default::default()
                    },
                    (!kind.is_empty()).then_some(kind),
                )
                .map(|_| ())
            }
            Page::Schemas => {
                let version = f
                    .value
                    .text
                    .trim()
                    .parse()
                    .context("Version must be a positive number")?;
                let schema = serde_json::from_str(&f.body.text).context("Invalid JSON schema")?;
                b.register_schema_as(f.name.text.trim(), version, &f.extra.text, &schema, actor)
                    .map(|_| ())
            }
            Page::Access => {
                let rules = board_local::access::Rules::parse(f.body.text.trim())?;
                b.set_grant(actor, None, f.name.text.trim(), &rules)
                    .map(|_| ())
            }
            _ => bail!("No active form"),
        });
        if self.result(result).is_some() {
            self.desk.enter(self.page);
            self.refresh();
        }
    }

    pub fn remove_admin(&mut self, key: &str) {
        let result = self.db().and_then(|db| {
            ensure!(
                self.desk.confirm.as_deref() == Some(key),
                "Confirmation required"
            );
            match self.page {
                Page::Watchers => automation::remove_watcher_as(db, &self.actor, key),
                Page::Access | Page::Workspace if self.desk.access.registry() => {
                    if let Some(id) = key.strip_prefix("grant-") {
                        db.revoke_grant(&self.actor, id.parse()?)
                    } else {
                        db.revoke_credential(
                            &self.actor,
                            key.strip_prefix("credential-")
                                .context("Unknown credential")?,
                        )
                    }
                }
                _ => bail!("No active resource"),
            }
        });
        if self.result(result).is_some() {
            self.desk.confirm = None;
            self.refresh();
        }
    }
}
