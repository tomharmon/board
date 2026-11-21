use crate::errors::Kind::{Conflict, Forbidden, Gone, Invalid, Missing};
use crate::{Block, access::Action};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, time::Duration};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS channels(name TEXT PRIMARY KEY NOT NULL);
CREATE TABLE IF NOT EXISTS messages (
 id INTEGER PRIMARY KEY, channel TEXT NOT NULL REFERENCES channels(name), actor TEXT NOT NULL,
 text TEXT NOT NULL, re INTEGER REFERENCES messages(id), reply_to INTEGER REFERENCES messages(id),
 created_at TEXT NOT NULL, revision INTEGER NOT NULL, deleted INTEGER NOT NULL DEFAULT 0,
 blocks TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX IF NOT EXISTS messages_by_thread ON messages(re,id);
CREATE INDEX IF NOT EXISTS messages_by_channel ON messages(channel,id);
CREATE INDEX IF NOT EXISTS messages_by_actor ON messages(actor,id);
CREATE TABLE IF NOT EXISTS private_messages(id INTEGER PRIMARY KEY REFERENCES messages(id));
CREATE TABLE IF NOT EXISTS mentions(message_id INTEGER NOT NULL REFERENCES messages(id), handle TEXT NOT NULL, PRIMARY KEY(message_id,handle));
CREATE INDEX IF NOT EXISTS mentions_by_handle ON mentions(handle,message_id);
CREATE TABLE IF NOT EXISTS labels(message_id INTEGER NOT NULL REFERENCES messages(id), key TEXT NOT NULL, value TEXT NOT NULL, PRIMARY KEY(message_id,key));
CREATE INDEX IF NOT EXISTS labels_by_value ON labels(key,value,message_id);
CREATE TABLE IF NOT EXISTS reactions(message_id INTEGER NOT NULL REFERENCES messages(id), emoji TEXT NOT NULL, actor TEXT NOT NULL, PRIMARY KEY(message_id,emoji,actor));
CREATE TABLE IF NOT EXISTS attachments(id INTEGER PRIMARY KEY AUTOINCREMENT, message_id INTEGER NOT NULL REFERENCES messages(id), name TEXT NOT NULL, size INTEGER NOT NULL, bytes BLOB NOT NULL);
CREATE INDEX IF NOT EXISTS attachments_by_message ON attachments(message_id);
CREATE TABLE IF NOT EXISTS events(cursor INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, actor TEXT NOT NULL, message_id INTEGER, created_at TEXT NOT NULL, payload TEXT NOT NULL, message TEXT, previous TEXT, root_labels TEXT NOT NULL DEFAULT '{}');
CREATE INDEX IF NOT EXISTS events_by_message ON events(message_id,cursor DESC);
CREATE TABLE IF NOT EXISTS write_keys(actor TEXT NOT NULL, key TEXT NOT NULL, fingerprint BLOB NOT NULL, message_id INTEGER NOT NULL REFERENCES messages(id), PRIMARY KEY(actor,key));
CREATE TABLE IF NOT EXISTS schemas(name TEXT NOT NULL, version INTEGER NOT NULL, description TEXT NOT NULL, definition TEXT NOT NULL, PRIMARY KEY(name,version));
";

// V1 let different authors claim the same string. Keep every historical row and
// event; the earliest successful write owns the upgraded board-wide replay key.
const REPLAY_KEYS: &str = "
CREATE TABLE IF NOT EXISTS client_posts(key TEXT PRIMARY KEY NOT NULL,message_id INTEGER NOT NULL REFERENCES messages(id));
CREATE TABLE IF NOT EXISTS client_batches(key TEXT PRIMARY KEY NOT NULL,actor TEXT NOT NULL,fingerprint BLOB NOT NULL,ids TEXT NOT NULL);
INSERT OR IGNORE INTO client_posts(key,message_id) SELECT key,message_id FROM write_keys ORDER BY message_id;
INSERT OR IGNORE INTO client_batches(key,actor,fingerprint,ids)
 SELECT key,actor,fingerprint,ids FROM task_requests t
 WHERE NOT EXISTS(SELECT 1 FROM write_keys w WHERE w.actor=t.actor AND w.key=t.key
 AND w.message_id=json_extract(t.ids,'$[0]'))
 ORDER BY json_extract(t.ids,'$[0]');
";

pub struct Board {
    pub(crate) conn: Connection,
    pub(crate) session: crate::access::Session,
    pub(crate) request_avatar: Option<(String, String)>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Attachment {
    pub id: i64,
    #[serde(default)]
    pub key: String,
    #[serde(default = "crate::files::binary_type")]
    pub content_type: String,
    pub name: String,
    pub size: i64,
}

pub const MAX_ATTACHMENTS: usize = 10;
pub const MAX_ATTACHMENT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Message {
    pub id: i64,
    pub re: Option<i64>,
    pub reply_to: Option<i64>,
    pub channel: String,
    pub actor: String,
    #[serde(default)]
    pub author: Option<crate::ActorEntry>,
    #[serde(default)]
    pub from: Option<String>,
    pub text: String,
    pub created_at: String,
    #[serde(default)]
    pub edited_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<crate::MentionResolution>,
    pub revision: i64,
    pub deleted: bool,
    /// This message (and, for a root, its entire thread) never leaves through a connector.
    #[serde(default)]
    pub no_sync: bool,
    pub labels: BTreeMap<String, String>,
    pub reactions: BTreeMap<String, Vec<String>>,
    pub attachments: Vec<Attachment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_notices: Vec<crate::sources::FileNotice>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<crate::links::Link>,
    pub blocks: Vec<Block>,
    /// Last change to this message. Unlike revision, this includes metadata updates.
    #[serde(default)]
    pub version: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<crate::automation::Task>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<crate::automation::ScheduledSend>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<crate::sources::Copy>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PostOptions {
    #[serde(default)]
    pub no_sync: bool,
    pub key: Option<String>,
    pub after: Option<i64>,
    pub labels: BTreeMap<String, String>,
    pub files: Vec<(String, Vec<u8>)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_ids: Vec<i64>,
}

pub fn validate_channel(s: &str) -> Result<()> {
    ensure!(
        s.len() <= 2048
            && s.split('/').all(|seg| seg
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
                && seg.bytes().all(|b| b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || matches!(b, b'_' | b'-'))),
        "invalid channel path: {s}"
    );
    Ok(())
}

fn validate_key(s: &str) -> Result<()> {
    validate_label_key(s)?;
    ensure!(
        !s.starts_with("board."),
        Invalid.error("reserved_label", format!("reserved label: {s}"))
    );
    Ok(())
}
pub(crate) fn validate_label_key(s: &str) -> Result<()> {
    ensure!(
        s.len() <= 64
            && s.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
            && s.bytes().all(|b| b.is_ascii_lowercase()
                || b.is_ascii_digit()
                || matches!(b, b'_' | b'-' | b'.')),
        "invalid label key: {s}"
    );
    ensure!(
        !matches!(s, "constructor" | "prototype"),
        "invalid label key: {s}"
    );
    Ok(())
}
pub fn validate_actor(s: &str) -> Result<()> {
    ensure!(
        s.len() <= 256
            && s.split('/').count() <= 2
            && s.split('/')
                .all(|part| !part.trim().is_empty() && part.trim() == part)
            && !s.chars().any(char::is_control),
        "invalid actor: expected a person or person/profile"
    );
    Ok(())
}
fn validate_text(text: &str, attached: bool) -> Result<()> {
    ensure!(!text.contains('\0'), "message contains NUL");
    ensure!(
        text.encode_utf16().count() <= 10_000,
        "message exceeds 10000 characters"
    );
    ensure!(
        attached || !text.trim().is_empty(),
        "message needs text or an attachment"
    );
    Ok(())
}
pub fn validate_post(channel: &str, actor: &str, text: &str, opts: &PostOptions) -> Result<()> {
    validate_channel(channel)?;
    validate_actor(actor)?;
    validate_text(
        text,
        !opts.files.is_empty() || !opts.attachment_ids.is_empty(),
    )?;
    if let Some(k) = &opts.key {
        validate_client_key(k)?;
    }
    ensure!(
        opts.files.len() + opts.attachment_ids.len() <= MAX_ATTACHMENTS,
        "at most {MAX_ATTACHMENTS} attachments"
    );
    for (name, data) in &opts.files {
        ensure!(
            data.len() <= MAX_ATTACHMENT_BYTES,
            "attachment exceeds 4 MiB: {name}"
        );
        crate::files::valid_name(name)?;
    }
    for (k, v) in &opts.labels {
        validate_key(k)?;
        validate_value(v)?;
    }
    Ok(())
}
fn validate_value(v: &str) -> Result<()> {
    ensure!(
        v.encode_utf16().count() <= 256 && !v.chars().any(char::is_control),
        "label values must be at most 256 characters without controls"
    );
    Ok(())
}

pub(crate) fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub(crate) fn validate_client_key(key: &str) -> Result<()> {
    ensure!(
        !key.is_empty() && !key.chars().any(char::is_control) && key.chars().count() <= 200,
        Invalid.error(
            "invalid_id",
            "client key must have 1-200 characters without controls"
        )
    );
    Ok(())
}

const APPLICATION_ID: i64 = 0x424f4152;
const DB_VERSION: i64 = 11;
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

fn enable_wal(conn: &Connection) -> Result<()> {
    let start = std::time::Instant::now();
    // Switching a new database from DELETE to WAL may return BUSY without invoking
    // SQLite's normal busy handler when two first-time openers upgrade together.
    loop {
        match conn.pragma_update(None, "journal_mode", "WAL") {
            Ok(()) => return Ok(()),
            Err(err)
                if matches!(
                    err.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
                ) && start.elapsed() < BUSY_TIMEOUT =>
            {
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(err) => return Err(err).context("enable WAL"),
        }
    }
}

/// Prepare a real database path without closing a raw descriptor to a published database.
/// On Unix closing *any* descriptor to that inode would release this process's SQLite locks.
pub fn prepare_sqlite_file(path: &Path) -> Result<()> {
    use std::io::ErrorKind;
    match std::fs::symlink_metadata(path) {
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // NamedTempFile is private (0600 on Unix). Drop its FD *before* publication:
    // another thread may open the final path with SQLite as soon as it exists.
    let closed = tempfile::NamedTempFile::new_in(parent)?.into_temp_path();
    match closed.persist_noclobber(path) {
        Ok(()) => Ok(()),
        Err(error) if error.error.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error).with_context(|| format!("prepare {}", path.display())),
    }
}

impl Board {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if path == Path::new(":memory:") {
            return Self::memory();
        }
        if let Some(p) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(p)?;
        }
        prepare_sqlite_file(path)?;
        // If another process removed the prepared path, fail rather than creating
        // a replacement with ordinary process permissions behind a live connection.
        let flags =
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
        Self::from_conn(
            Connection::open_with_flags(path, flags)
                .with_context(|| format!("open {}", path.display()))?,
        )
    }
    pub fn memory() -> Result<Self> {
        Self::from_conn(Connection::open_in_memory()?)
    }
    fn from_conn(conn: Connection) -> Result<Self> {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        use rusqlite::functions::FunctionFlags;
        conn.create_scalar_function(
            "board_lower",
            1,
            FunctionFlags::SQLITE_UTF8
                | FunctionFlags::SQLITE_DETERMINISTIC
                | FunctionFlags::SQLITE_INNOCUOUS,
            |args| Ok(args.get::<String>(0)?.to_lowercase()),
        )?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let app: i64 = conn.pragma_query_value(None, "application_id", |r| r.get(0))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        ensure!(app == 0 || app == APPLICATION_ID, "not a Board database");
        ensure!(
            version <= DB_VERSION,
            "this board was created by a newer version"
        );
        if app == 0 {
            let known:bool=conn.query_row("SELECT NOT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%') OR
                (SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('messages','events','channels','write_keys','tasks','schemas'))=6",[],|r|r.get(0))?;
            ensure!(known, "not a Board database");
        }
        enable_wal(&conn)?;
        if version == DB_VERSION && app == APPLICATION_ID {
            return Ok(Self {
                conn,
                session: Default::default(),
                request_avatar: None,
            });
        }
        let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)?;
        // Another opener may have migrated while this connection waited for the writer.
        let locked_version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        ensure!(
            locked_version <= DB_VERSION,
            "this board was created by a newer version"
        );
        if locked_version == DB_VERSION
            && tx.pragma_query_value(None, "application_id", |r| r.get::<_, i64>(0))?
                == APPLICATION_ID
        {
            tx.commit()?;
            return Ok(Self {
                conn,
                session: Default::default(),
                request_avatar: None,
            });
        }
        tx.execute_batch(SCHEMA)?;
        crate::schemas::init(&tx)?;
        crate::automation::init_automation(&tx)?;
        tx.execute_batch(REPLAY_KEYS)?;
        crate::access::init(&tx)?;
        crate::sources::init(&tx)?;
        crate::policy::init(&tx)?;
        crate::presence::init(&tx)?;
        crate::files::init(&tx)?;
        crate::actors::init(&tx)?;
        tx.pragma_update(None, "application_id", APPLICATION_ID)?;
        tx.pragma_update(None, "user_version", DB_VERSION)?;
        tx.commit()?;
        Ok(Self {
            conn,
            session: Default::default(),
            request_avatar: None,
        })
    }
    pub(crate) fn write<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        if let Some((actor, avatar)) = &self.request_avatar {
            let a = self.access_on(&tx)?;
            a.actor(actor)?;
            if tx.execute("INSERT INTO actor_avatars VALUES(?,?) ON CONFLICT(id) DO UPDATE SET url=excluded.url WHERE url<>excluded.url",params![actor,avatar])?>0 {
                Self::event_tx(&tx,"actor.updated",actor,None,json!({"id":actor}))?;
            }
        }
        let v = f(&tx)?;
        tx.commit()?;
        Ok(v)
    }
    pub fn create_channel(&self, name: &str) -> Result<()> {
        self.create_channel_as(name, &crate::local::actor_name())
            .map(|_| ())
    }
    pub fn create_channel_as(&self, name: &str, actor: &str) -> Result<bool> {
        validate_channel(name)?;
        validate_actor(actor)?;
        self.write(|tx| {
            let access = self.access_on(tx)?;
            access.actor(actor)?;
            access.require(Action::Read, name)?;
            access.require(Action::Post, name)?;
            let new = tx.execute("INSERT OR IGNORE INTO channels VALUES (?)", [name])? != 0;
            if new {
                Self::event_tx(tx, "channel.created", actor, None, json!({"name":name}))?;
            }
            Ok(new)
        })
    }
    /// Changes when a different SQLite connection commits; useful for live inspectors.
    pub fn data_version(&self) -> Result<i64> {
        self.access_on(&self.conn)?;
        Ok(self
            .conn
            .pragma_query_value(None, "data_version", |r| r.get(0))?)
    }
    pub fn get(&self, id: i64) -> Result<Message> {
        let tx = self.conn.unchecked_transaction()?;
        let access = self.access_on(&tx)?;
        access.message(&tx, id, Action::Read)?;
        let m = access.view(&tx, Self::get_on(&tx, id)?)?;
        tx.commit()?;
        Ok(m)
    }
    /// Recover a keyed message after a lost response without disclosing another writer's keys.
    pub fn posted(&self, actor: &str, key: &str) -> Result<Message> {
        validate_actor(actor)?;
        validate_client_key(key)?;
        let tx = self.conn.unchecked_transaction()?;
        let access = self.access_on(&tx)?;
        access.actor(actor)?;
        let id = tx.query_row(
            "SELECT m.id FROM client_posts k JOIN messages m ON m.id=k.message_id WHERE k.key=? AND m.actor=?",
            [key, actor],
            |r| r.get(0),
        ).optional()?.context(Missing.error("post_not_found", "no post of yours has that client key"))?;
        let message = Self::get_on(&tx, id)?;
        let message = if access.allows(Action::Read, &message.channel) {
            access.view(&tx, message)?
        } else {
            ensure!(
                access.allows(Action::Post, &message.channel) && access.blind_on(&tx, id)?,
                Missing.error("post_not_found", "no post of yours has that client key")
            );
            access.original(&tx, id)?
        };
        tx.commit()?;
        Ok(message)
    }
    pub(crate) fn get_on(c: &Connection, id: i64) -> Result<Message> {
        let (channel,actor,text,re,reply_to,created_at,revision,deleted,blocks,no_sync) = c.query_row(
            "SELECT channel,actor,text,re,reply_to,created_at,revision,deleted,blocks,EXISTS(SELECT 1 FROM private_messages p WHERE p.id=m.id) FROM messages m WHERE id=?",
            [id], |r| Ok((r.get(0)?,r.get::<_,String>(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get::<_,String>(8)?,r.get(9)?)),
        ).optional()?.with_context(||Missing.error("message_not_found",format!("message #{id} not found")))?;
        let labels = labels_on(c, id)?;
        let mut reactions: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for row in c
            .prepare("SELECT emoji,actor FROM reactions WHERE message_id=? ORDER BY rowid")?
            .query_map([id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
        {
            let (emoji, who) = row?;
            reactions.entry(emoji).or_default().push(who);
        }
        let attachments = crate::files::on_message(c, id)?;
        let edited_at=c.query_row("SELECT created_at FROM events WHERE message_id=? AND kind='message.edited' ORDER BY cursor DESC LIMIT 1",[id],|r|r.get(0)).optional()?;
        let version=c.query_row("SELECT coalesce(max(cursor),?1) FROM events WHERE message_id=?1 AND (kind IN ('message.edited','message.deleted','message.links','attachment.added','attachment.unavailable','labels.changed','reaction.added','reaction.removed') OR kind LIKE 'task.%')",[id],|r|r.get(0))?;
        let task = if labels.contains_key("board.task") || deleted {
            crate::automation::task_optional(c, id)?
        } else {
            None
        };
        let origin = crate::sources::origin_on(c, id)?;
        Ok(Message {
            id,
            re,
            reply_to,
            channel,
            author: Some(crate::actors::card_on(c, &actor)?),
            actor,
            from: origin.as_ref().map(|o| o.source.clone()),
            text,
            created_at,
            edited_at,
            mentions: vec![],
            revision,
            deleted,
            no_sync,
            labels,
            reactions,
            attachments,
            links: if deleted {
                Vec::new()
            } else {
                crate::links::on(c, id)?
            },
            file_notices: if deleted {
                Vec::new()
            } else {
                crate::sources::file_notices_on(c, id)?
            },
            blocks: serde_json::from_str(&blocks)?,
            version,
            task,
            schedule: crate::automation::send_info(c, id)?,
            origin,
        })
    }
    pub fn post(
        &self,
        channel: &str,
        actor: &str,
        text: &str,
        opts: &PostOptions,
    ) -> Result<Message> {
        Ok(self.post_with_outcome(channel, actor, text, opts)?.0)
    }
    pub fn reply(&self, id: i64, actor: &str, text: &str, opts: &PostOptions) -> Result<Message> {
        Ok(self.reply_with_outcome(id, actor, text, opts)?.0)
    }
    pub fn post_with_outcome(
        &self,
        channel: &str,
        actor: &str,
        text: &str,
        opts: &PostOptions,
    ) -> Result<(Message, bool)> {
        self.post_guarded(channel, actor, text, opts, None)
    }
    pub fn post_composer(
        &self,
        channel: &str,
        actor: &str,
        text: &str,
        opts: &PostOptions,
        stamp: &str,
    ) -> Result<Message> {
        Ok(self
            .post_guarded(channel, actor, text, opts, Some(stamp))?
            .0)
    }
    fn post_guarded(
        &self,
        channel: &str,
        actor: &str,
        text: &str,
        opts: &PostOptions,
        stamp: Option<&str>,
    ) -> Result<(Message, bool)> {
        self.write(|tx| {
            if let Some(stamp) = stamp {
                let draft = self.check_composer_options_on(tx, actor, "new", text, opts, None)?;
                ensure!(
                    draft.channel == channel && draft.reply_to.is_none(),
                    Conflict.error("draft_changed", "draft destination no longer matches")
                );
                self.consume_composer_on(tx, actor, "new", stamp)?;
            }
            let a = self.access_on(tx)?;
            a.actor(actor)?;
            a.require(Action::Post, channel)?;
            a.files(tx, actor, &opts.attachment_ids)?;
            if !opts.labels.is_empty() {
                a.require(Action::Label, channel)?;
            }
            let (text, mentions) = crate::actors::settle(tx, &a, actor, None, text)?;
            let (m, created) = Self::post_tx_with_outcome(tx, channel, actor, &text, opts)?;
            if a.allows(Action::Read, channel) {
                return Ok((
                    crate::actors::write_view(a.view(tx, m)?, &text, mentions, false),
                    created,
                ));
            }
            if created {
                tx.execute("INSERT INTO blind_posts VALUES(?)", [m.id])?;
            }
            ensure!(
                a.blind_on(tx, m.id)?,
                Conflict.error("duplicate_id", "client key belongs to a different write")
            );
            Ok((
                crate::actors::write_view(a.original(tx, m.id)?, &text, mentions, true),
                created,
            ))
        })
    }
    pub fn reply_with_outcome(
        &self,
        id: i64,
        actor: &str,
        text: &str,
        opts: &PostOptions,
    ) -> Result<(Message, bool)> {
        self.reply_guarded(id, actor, text, opts, None)
    }
    pub fn reply_composer(
        &self,
        id: i64,
        actor: &str,
        text: &str,
        opts: &PostOptions,
        guard: (&str, &str),
    ) -> Result<Message> {
        Ok(self.reply_guarded(id, actor, text, opts, Some(guard))?.0)
    }
    fn reply_guarded(
        &self,
        id: i64,
        actor: &str,
        text: &str,
        opts: &PostOptions,
        guard: Option<(&str, &str)>,
    ) -> Result<(Message, bool)> {
        self.write(|tx| {
            if let Some((scope, stamp)) = guard {
                let draft = self.check_composer_options_on(tx, actor, scope, text, opts, None)?;
                let root = self.draft_scope(tx, actor, scope)?;
                ensure!(
                    root.is_some() && draft.reply_to.or(root) == Some(id),
                    Conflict.error("draft_changed", "draft reply target no longer matches")
                );
                self.consume_composer_on(tx, actor, scope, stamp)?;
            }
            let a = self.access_on(tx)?;
            a.actor(actor)?;
            let channel = a.message(tx, id, Action::Post)?;
            a.files(tx, actor, &opts.attachment_ids)?;
            if !opts.labels.is_empty() {
                a.require(Action::Label, &channel)?;
            }
            let (text, mentions) = crate::actors::settle(tx, &a, actor, Some(id), text)?;
            let (m, created) = Self::insert_reply(tx, id, actor, &text, opts)?;
            Ok((
                crate::actors::write_view(a.view(tx, m)?, &text, mentions, false),
                created,
            ))
        })
    }
    pub(crate) fn post_tx(
        tx: &Transaction,
        channel: &str,
        actor: &str,
        text: &str,
        opts: &PostOptions,
    ) -> Result<Message> {
        Ok(Self::post_tx_with_outcome(tx, channel, actor, text, opts)?.0)
    }
    pub(crate) fn post_tx_with_outcome(
        tx: &Transaction,
        channel: &str,
        actor: &str,
        text: &str,
        opts: &PostOptions,
    ) -> Result<(Message, bool)> {
        Self::insert(tx, channel, None, actor, text, opts)
    }
    pub(crate) fn reply_tx(
        tx: &Transaction,
        id: i64,
        actor: &str,
        text: &str,
        opts: &PostOptions,
    ) -> Result<Message> {
        Ok(Self::insert_reply(tx, id, actor, text, opts)?.0)
    }
    fn insert_reply(
        tx: &Transaction,
        id: i64,
        actor: &str,
        text: &str,
        opts: &PostOptions,
    ) -> Result<(Message, bool)> {
        let parent = Self::get_on(tx, id)?;
        Self::insert(
            tx,
            &parent.channel,
            Some((parent.re.unwrap_or(id), id)),
            actor,
            text,
            opts,
        )
    }
    fn insert(
        tx: &Transaction,
        channel: &str,
        parent: Option<(i64, i64)>,
        actor: &str,
        text: &str,
        opts: &PostOptions,
    ) -> Result<(Message, bool)> {
        validate_post(channel, actor, text, opts)?;
        if let Some(key) = &opts.key {
            crate::automation::authorize_callback_key(tx, actor, key)?;
        }
        let re = parent.map(|p| p.0);
        let reply_to = parent.and_then(|(root, p)| (p != root).then_some(p));
        ensure!(
            parent.is_some() || opts.after.is_none(),
            Invalid.error("invalid_after", "after is only valid for replies")
        );
        if let Some(key) = &opts.key
            && let Some(id) = tx
                .query_row(
                    "SELECT message_id FROM client_posts WHERE key=?",
                    [key],
                    |r| r.get(0),
                )
                .optional()?
        {
            let original = Self::get_on(tx, id)?;
            ensure!(
                original.actor == actor
                    && original.channel == channel
                    && original.re == re
                    && original.reply_to == reply_to,
                Conflict.error("duplicate_id", "client key belongs to a different write")
            );
            return Ok((original, false));
        }
        if let Some(after) = opts.after {
            let (root, _) = parent.context("after is only valid for replies")?;
            let valid: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE id=?1 AND (id=?2 OR re=?2))",
                [after, root],
                |r| r.get(0),
            )?;
            ensure!(
                valid,
                Invalid.error("invalid_after", "expected a message in this thread")
            );
            let latest: i64 = tx.query_row(
                "SELECT max(id) FROM messages WHERE id=?1 OR re=?1",
                [root],
                |r| r.get(0),
            )?;
            ensure!(
                latest == after,
                Conflict.error(
                    "newer_message",
                    format!("latest message is #{latest}, not #{after}")
                )
            );
        }
        let attached = !opts.files.is_empty() || !opts.attachment_ids.is_empty();
        let patch = crate::policy::filter(
            tx,
            crate::policy::Write {
                channel,
                actor,
                id: None,
                root: re,
                text,
                labels: &opts.labels,
                attached,
                no_sync: opts.no_sync,
            },
        )?;
        let (text, labels) = match &patch {
            Some((t, l)) => (t.as_str(), l),
            None => (text, &opts.labels),
        };
        if patch.is_some() {
            validate_text(text, attached)?;
            for (k, v) in labels {
                validate_key(k)?;
                validate_value(v)?;
            }
        }
        let blocks = Self::blocks_on(tx, text)?;
        tx.execute("INSERT OR IGNORE INTO channels VALUES (?)", [channel])?;
        let ts = now();
        tx.execute(
            "INSERT INTO events(kind,actor,created_at,payload) VALUES('message.posted',?,?,'{}')",
            params![actor, ts],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute("INSERT INTO messages(id,channel,actor,text,re,reply_to,created_at,revision,blocks) VALUES(?1,?2,?3,?4,?5,?6,?7,?1,?8)",params![id,channel,actor,text,re,reply_to,ts,serde_json::to_string(&blocks)?])?;
        if opts.no_sync {
            tx.execute("INSERT INTO private_messages VALUES(?)", [id])?;
        }
        for (k, v) in labels {
            tx.execute("INSERT INTO labels VALUES(?,?,?)", params![id, k, v])?;
        }
        update_mentions(tx, id, text)?;
        crate::links::capture(tx, id, channel, text)?;
        crate::files::attach(tx, id, actor, channel, opts)?;
        if let Some(key) = &opts.key {
            tx.execute(
                "INSERT INTO client_posts(key,message_id) VALUES (?,?)",
                params![key, id],
            )?;
        }
        let m = Self::get_on(tx, id)?;
        Self::snapshot(tx, id, &m, None)?;
        Ok((m, true))
    }
    pub(crate) fn snapshot(
        tx: &Transaction,
        cursor: i64,
        m: &Message,
        previous: Option<&Message>,
    ) -> Result<()> {
        let root_labels = match m.re {
            Some(root) => labels_on(tx, root)?,
            None => BTreeMap::new(),
        };
        tx.execute(
            "UPDATE events SET message_id=?,message=?,previous=?,root_labels=? WHERE cursor=?",
            params![
                m.id,
                serde_json::to_string(m)?,
                previous.map(serde_json::to_string).transpose()?,
                serde_json::to_string(&root_labels)?,
                cursor
            ],
        )?;
        Ok(())
    }
    pub(crate) fn event_tx(
        tx: &Transaction,
        kind: &str,
        actor: &str,
        message_id: Option<i64>,
        payload: Value,
    ) -> Result<i64> {
        tx.execute(
            "INSERT INTO events(kind,actor,message_id,created_at,payload) VALUES(?,?,?,?,?)",
            params![
                kind,
                actor,
                message_id,
                now(),
                serde_json::to_string(&payload)?
            ],
        )?;
        let seq = tx.last_insert_rowid();
        if let Some(id) = message_id {
            Self::snapshot(tx, seq, &Self::get_on(tx, id)?, None)?;
        }
        Ok(seq)
    }
    pub(crate) fn changed(
        tx: &Transaction,
        kind: &str,
        who: &str,
        before: &Message,
        payload: Value,
        revision: bool,
    ) -> Result<Message> {
        let id = before.id;
        tx.execute(
            "INSERT INTO events(kind,actor,message_id,created_at,payload) VALUES(?,?,?,?,?)",
            params![kind, who, id, now(), serde_json::to_string(&payload)?],
        )?;
        let seq = tx.last_insert_rowid();
        if revision {
            tx.execute(
                "UPDATE messages SET revision=? WHERE id=?",
                params![seq, id],
            )?;
        }
        let m = Self::get_on(tx, id)?;
        Self::snapshot(tx, seq, &m, Some(before))?;
        Ok(m)
    }
    pub fn edit(&self, id: i64, actor: &str, text: &str, revision: Option<i64>) -> Result<Message> {
        self.write(|tx| {
            let a = self.access_on(tx)?;
            let blind = a.own_post(tx, id, actor)?;
            let (text, mentions) = crate::actors::settle(tx, &a, actor, Some(id), text)?;
            let m = Self::edit_tx(tx, id, actor, &text, revision)?;
            if !blind {
                return Ok(crate::actors::write_view(
                    a.view(tx, m)?,
                    &text,
                    mentions,
                    false,
                ));
            }
            let mut own = a.original(tx, id)?;
            own.text = crate::prose::redact_exact(&m.text, &own.actor);
            own.blocks = m.blocks;
            own.revision = m.revision;
            own.version = m.version;
            own.edited_at = m.edited_at;
            Ok(crate::actors::write_view(own, &text, mentions, true))
        })
    }
    pub(crate) fn edit_tx(
        tx: &Transaction<'_>,
        id: i64,
        actor: &str,
        text: &str,
        revision: Option<i64>,
    ) -> Result<Message> {
        let old = Self::get_on(tx, id)?;
        may_edit(&old, actor, revision)?;
        ensure!(!old.deleted, Gone.error("deleted", "message removed"));
        let patch = crate::policy::filter(
            tx,
            crate::policy::Write {
                channel: &old.channel,
                actor,
                id: Some(id),
                root: Some(old.re.unwrap_or(old.id)),
                text,
                labels: &old.labels,
                attached: !old.attachments.is_empty(),
                no_sync: old.no_sync,
            },
        )?;
        let text = patch.as_ref().map_or(text, |(s, _)| s.as_str());
        validate_text(text, !old.attachments.is_empty())?;
        let blocks = Self::blocks_on(tx, text)?;
        tx.execute(
            "UPDATE messages SET text=?,blocks=? WHERE id=?",
            params![text, serde_json::to_string(&blocks)?, id],
        )?;
        update_mentions(tx, id, text)?;
        crate::links::capture(tx, id, &old.channel, text)?;
        Self::changed(tx, "message.edited", actor, &old, json!({}), true)?;
        if old.re.is_none() {
            crate::automation::on_root_edit(tx, id, actor)?;
        }
        Self::get_on(tx, id)
    }
    pub fn delete(&self, id: i64, actor: &str, revision: Option<i64>) -> Result<Message> {
        self.write(|tx| {
            let a = self.access_on(tx)?;
            let blind = a.own_post(tx, id, actor)?;
            let mut m = Self::delete_tx(tx, id, actor, revision)?;
            if blind {
                m.task = None;
                m.schedule = None;
            }
            a.view(tx, m)
        })
    }
    pub(crate) fn delete_tx(
        tx: &Transaction<'_>,
        id: i64,
        actor: &str,
        revision: Option<i64>,
    ) -> Result<Message> {
        let old = Self::get_on(tx, id)?;
        may_edit(&old, actor, revision)?;
        if old.deleted {
            return Ok(old);
        }
        let before = if old.task.is_some() {
            crate::automation::on_root_delete(tx, id, actor)?;
            Self::get_on(tx, id)?
        } else {
            old
        };
        tx.execute(
            "UPDATE messages SET text='',deleted=1,blocks='[]' WHERE id=?",
            [id],
        )?;
        for table in [
            "message_files",
            "labels",
            "reactions",
            "mentions",
            "exact_mentions",
        ] {
            tx.execute(&format!("DELETE FROM {table} WHERE message_id=?"), [id])?;
        }
        Self::changed(tx, "message.deleted", actor, &before, json!({}), true)?;
        crate::automation::cancel_for_root(tx, id, actor)?;
        Self::get_on(tx, id)
    }
    pub fn label(&self, id: i64, actor: &str, key: &str, value: Option<&str>) -> Result<Message> {
        let mut set = BTreeMap::new();
        let mut unset = Vec::new();
        if let Some(v) = value {
            set.insert(key.to_owned(), v.to_owned());
        } else {
            unset.push(key.to_owned());
        }
        self.labels(id, actor, &set, &unset, &BTreeMap::new())
    }
    pub fn labels(
        &self,
        id: i64,
        actor: &str,
        set: &BTreeMap<String, String>,
        unset: &[String],
        conditions: &BTreeMap<String, Option<String>>,
    ) -> Result<Message> {
        self.write(|tx| {
            let a = self.access_on(tx)?;
            a.actor(actor)?;
            a.message(tx, id, Action::Label)?;
            a.view(tx, Self::labels_tx(tx, id, actor, set, unset, conditions)?)
        })
    }
    pub(crate) fn labels_tx(
        tx: &Transaction<'_>,
        id: i64,
        actor: &str,
        set: &BTreeMap<String, String>,
        unset: &[String],
        conditions: &BTreeMap<String, Option<String>>,
    ) -> Result<Message> {
        validate_actor(actor)?;
        ensure!(conditions.len() <= 16, "at most 16 label conditions");
        for (key, value) in conditions {
            validate_label_key(key)?;
            ensure!(
                value
                    .as_ref()
                    .is_none_or(|s| s.encode_utf16().count() <= 256 && !s.contains('\0')),
                "invalid expected label value"
            );
        }
        for k in set.keys().chain(unset.iter()) {
            validate_key(k)?;
        }
        for (k, v) in set {
            validate_value(v)?;
            ensure!(!unset.contains(k), "key both set and unset: {k}");
        }
        let old = Self::get_on(tx, id)?;
        ensure!(!old.deleted, Gone.error("deleted", "message removed"));
        for (k, v) in conditions {
            ensure!(
                old.labels.get(k) == v.as_ref(),
                Conflict.error("label_conflict", k.clone())
            );
        }
        if set.iter().all(|(k, v)| old.labels.get(k) == Some(v))
            && unset.iter().all(|k| !old.labels.contains_key(k))
        {
            return Ok(old);
        }
        for (k, v) in set {
            tx.execute("INSERT INTO labels VALUES (?,?,?) ON CONFLICT(message_id,key) DO UPDATE SET value=excluded.value",params![id,k,v])?;
        }
        for k in unset {
            tx.execute(
                "DELETE FROM labels WHERE message_id=? AND key=?",
                params![id, k],
            )?;
        }
        Self::changed(
            tx,
            "labels.changed",
            actor,
            &old,
            json!({"set":set,"unset":unset}),
            false,
        )
    }
    pub fn react(&self, id: i64, actor: &str, emoji: &str, remove: bool) -> Result<Message> {
        self.write(|tx| {
            let a = self.access_on(tx)?;
            a.actor(actor)?;
            a.message(tx, id, Action::React)?;
            a.view(tx, Self::react_tx(tx, id, actor, emoji, remove)?)
        })
    }
    pub(crate) fn react_tx(
        tx: &Transaction<'_>,
        id: i64,
        actor: &str,
        emoji: &str,
        remove: bool,
    ) -> Result<Message> {
        validate_actor(actor)?;
        let emoji = crate::reactions::normalize(emoji)?;
        let old = Self::get_on(tx, id)?;
        ensure!(!old.deleted, Gone.error("deleted", "message removed"));
        let sql = if remove {
            "DELETE FROM reactions WHERE message_id=? AND emoji=? AND actor=?"
        } else {
            "INSERT OR IGNORE INTO reactions VALUES(?,?,?)"
        };
        if tx.execute(sql, params![id, emoji, actor])? == 0 {
            return Ok(old);
        }
        Self::changed(
            tx,
            if remove {
                "reaction.removed"
            } else {
                "reaction.added"
            },
            actor,
            &old,
            json!({"emoji":emoji}),
            false,
        )
    }
}

pub fn owned_by(owner: &str, actor: &str) -> bool {
    owner == actor
        || owner
            .split_once('/')
            .is_some_and(|(person, _)| person == actor)
}

fn may_edit(m: &Message, actor: &str, revision: Option<i64>) -> Result<()> {
    ensure!(
        owned_by(&m.actor, actor),
        Forbidden.error(
            "not_owned",
            "only the author or profile owner can change this message"
        )
    );
    if revision.is_some_and(|v| v != m.revision) {
        bail!(Conflict.error(
            "revision_conflict",
            format!("current revision is {}", m.revision)
        ));
    }
    Ok(())
}
pub(crate) fn labels_on(c: &Connection, id: i64) -> Result<BTreeMap<String, String>> {
    Ok(
        c.prepare("SELECT key,value FROM labels WHERE message_id=?")?
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?,
    )
}
pub(crate) fn mentions(text: &str) -> impl Iterator<Item = String> + '_ {
    crate::prose::tokens(text)
        .into_iter()
        .map(|t| crate::prose::term(t.value))
}
pub(crate) fn update_mentions(c: &Connection, id: i64, text: &str) -> Result<()> {
    c.execute("DELETE FROM mentions WHERE message_id=?", [id])?;
    c.execute("DELETE FROM exact_mentions WHERE message_id=?", [id])?;
    for m in crate::prose::tokens(text) {
        c.execute(
            "INSERT OR IGNORE INTO mentions VALUES(?,?)",
            params![id, crate::prose::term(m.value)],
        )?;
        if m.exact {
            c.execute(
                "INSERT OR IGNORE INTO exact_mentions VALUES(?,?)",
                params![id, m.value],
            )?;
        }
    }
    Ok(())
}
