use super::{now, write};
use crate::access::{Access, Action};
use crate::errors::Kind::{Conflict, Forbidden, Missing};
use crate::{Board, Event, Filter};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

#[derive(Debug, Serialize)]
pub struct Watcher {
    pub name: String,
    #[serde(skip)]
    key: String,
    pub filter: Filter,
    pub kind: Option<String>,
    pub all: bool,
    pub cursor: i64,
    /// Writes made as this watcher's profile don't feed back into it (except pre-v16 timed posts).
    pub run_as: Option<String>,
}

/// A pull consumer can lease a delivery instead of peeking. Only this receipt can ack the lease.
#[derive(Debug, Serialize)]
pub struct Delivery {
    pub watcher: String,
    pub event: Event,
    /// Stable across attempts and unique to the board, registration and event.
    pub trigger: String,
    pub receipt: String,
    pub lease_until: i64,
    /// Number of earlier tries; zero on the first delivery.
    pub attempt: i64,
    /// Number of tries including this lease; the callback invocation is one-based.
    pub attempts: i64,
}

#[derive(Debug, Serialize)]
pub struct WatchFailure {
    pub cursor: i64,
    pub thread_id: Option<i64>,
    pub attempts: i64,
    pub error: String,
}

pub enum DeliveryOutcome<'a> {
    Ack,
    Skip,
    Error(&'a str),
}

const MAX_ATTEMPTS: i64 = 5;
const MAX_INTERRUPTED_ATTEMPTS: i64 = 10;
mod outputs;
pub(crate) use outputs::authorize_callback_key;
pub use outputs::{callback_key, callback_write_key, start_callback};

pub(super) fn init(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS watcher_scan(
            name TEXT PRIMARY KEY REFERENCES watchers(name) ON DELETE CASCADE,
            scanned INTEGER NOT NULL, run_as TEXT
        );
        CREATE TABLE IF NOT EXISTS watcher_queue(
            name TEXT NOT NULL REFERENCES watchers(name) ON DELETE CASCADE,
            cursor INTEGER NOT NULL REFERENCES events(cursor),
            lane INTEGER NOT NULL, state TEXT NOT NULL DEFAULT 'pending',
            attempts INTEGER NOT NULL DEFAULT 0, retry_at INTEGER NOT NULL DEFAULT 0,
            receipt TEXT, lease_until INTEGER, offered INTEGER NOT NULL DEFAULT 0,
            last_receipt TEXT, error TEXT, PRIMARY KEY(name,cursor)
        );
        CREATE TABLE IF NOT EXISTS watcher_registry(
            internal TEXT PRIMARY KEY REFERENCES watchers(name) ON DELETE CASCADE,
            actor TEXT NOT NULL,name TEXT NOT NULL,credential TEXT NOT NULL, UNIQUE(actor,name));
        CREATE TABLE IF NOT EXISTS watcher_callback_runs(
            name TEXT NOT NULL REFERENCES watchers(name) ON DELETE CASCADE, cursor INTEGER NOT NULL, receipt TEXT NOT NULL,
            capability BLOB NOT NULL UNIQUE, actor TEXT NOT NULL, seq INTEGER NOT NULL,PRIMARY KEY(name,cursor));
        CREATE TABLE IF NOT EXISTS watcher_outputs(key TEXT PRIMARY KEY,actor TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS watcher_lane ON watcher_queue(name,lane,cursor) WHERE state='pending';
        CREATE INDEX IF NOT EXISTS watcher_pending ON watcher_queue(name,cursor) WHERE state='pending';
        CREATE TABLE IF NOT EXISTS watcher_verbose(name TEXT PRIMARY KEY REFERENCES watchers(name) ON DELETE CASCADE);
        CREATE TABLE IF NOT EXISTS watcher_filter_start(
            singleton INTEGER PRIMARY KEY CHECK(singleton=1),through INTEGER NOT NULL);
        INSERT OR IGNORE INTO watcher_filter_start SELECT 1,coalesce(max(cursor),0) FROM events;
        CREATE TABLE IF NOT EXISTS watcher_filter_hits(
            name TEXT NOT NULL REFERENCES watchers(name) ON DELETE CASCADE,
            cursor INTEGER NOT NULL REFERENCES events(cursor),PRIMARY KEY(name,cursor)
        ) WITHOUT ROWID;
        CREATE INDEX IF NOT EXISTS watcher_identity_filters ON watchers(kind,name)
            WHERE json_extract(filter,'$.by') IS NOT NULL OR json_extract(filter,'$.mentions') IS NOT NULL;
        INSERT OR IGNORE INTO watcher_scan(name,scanned)
            SELECT name,cursor FROM watchers;",
    )?;
    let legacy: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='watcher_delivery')",
        [],
        |r| r.get(0),
    )?;
    if legacy {
        // v1 had a single in-flight delivery. Preserve its receipt, deadline, and last
        // acknowledged cursor. Insert-or-ignore below will not replace it while catching up.
        conn.execute_batch(
            "INSERT OR IGNORE INTO watcher_queue(name,cursor,lane,attempts,receipt,lease_until)
            SELECT d.name,d.pending,coalesce(m.re,m.id,0),1,d.receipt,d.lease_until
            FROM watcher_delivery d JOIN events e ON e.cursor=d.pending LEFT JOIN messages m ON m.id=e.message_id;
          INSERT OR IGNORE INTO watcher_queue(name,cursor,lane,state,last_receipt)
            SELECT d.name,d.last_ack,coalesce(m.re,m.id,0),'acked',d.last_receipt
            FROM watcher_delivery d JOIN events e ON e.cursor=d.last_ack LEFT JOIN messages m ON m.id=e.message_id;
          DROP TABLE watcher_delivery;",
        )?;
    }
    Ok(())
}

/// A name, its owner and a credential's grants can all change before a pull.
/// Save only positive decisions, not message copies. A provisional task or source
/// post may replace its own snapshot inside this same transaction.
pub(crate) fn capture_filters(tx: &Transaction<'_>, cursor: i64) -> Result<()> {
    let mut stmt = tx.prepare_cached(
        "SELECT w.name,w.filter,r.credential FROM watcher_registry r JOIN watchers w ON w.name=r.internal
            WHERE w.kind IS NULL OR w.kind=(SELECT kind FROM events WHERE cursor=?1)
         UNION ALL
         SELECT w.name,w.filter,NULL FROM watchers w
            WHERE (json_extract(w.filter,'$.by') IS NOT NULL OR json_extract(w.filter,'$.mentions') IS NOT NULL)
              AND (w.kind IS NULL OR w.kind=(SELECT kind FROM events WHERE cursor=?1))
              AND NOT EXISTS(SELECT 1 FROM watcher_registry r WHERE r.internal=w.name)
         ORDER BY 3")?;
    let mut rows = stmt.query([cursor])?;
    let Some(mut row) = rows.next()? else {
        return Ok(());
    };
    let event = event_at(tx, cursor)?;
    let mut add = tx.prepare_cached("INSERT OR IGNORE INTO watcher_filter_hits VALUES(?,?)")?;
    let mut remove =
        tx.prepare_cached("DELETE FROM watcher_filter_hits WHERE name=? AND cursor=?")?;
    loop {
        // Only the credential that registered a watcher determines its event-time view.
        let credential: Option<String> = row.get(2)?;
        let access = match &credential {
            Some(id) => Board::credential_access_on(tx, id)?,
            None => Some(Access::owner()),
        };
        let mut audience = match access.as_ref() {
            Some(a) => crate::feed::allowed(tx, a, event.clone())?
                .map(|e| (crate::feed::EventResolver::new(tx, a), e)),
            None => None,
        };
        loop {
            let name: String = row.get(0)?;
            let matches = if let Some((resolver, e)) = &mut audience {
                let filter = serde_json::from_str(&row.get::<_, String>(1)?)?;
                resolver.resolve(&filter)?.matches(e)
            } else {
                false
            };
            if matches {
                add.execute(params![name, cursor])?;
            } else {
                remove.execute(params![name, cursor])?;
            }
            let Some(next) = rows.next()? else {
                return Ok(());
            };
            row = next;
            if row.get::<_, Option<String>>(2)? != credential {
                break;
            }
        }
    }
}

fn get(conn: &Connection, name: &str) -> Result<Watcher> {
    let (filter, kind, cursor, run_as): (String, Option<String>, i64, Option<String>) = conn
        .query_row(
            "SELECT w.filter,w.kind,w.cursor,s.run_as FROM watchers w LEFT JOIN watcher_scan s USING(name) WHERE w.name=?",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .with_context(|| Missing.error("watcher_not_found", "unknown watcher"))?;
    Ok(Watcher {
        name: conn
            .query_row(
                "SELECT name FROM watcher_registry WHERE internal=?",
                [name],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or_else(|| name.to_owned()),
        key: name.to_owned(),
        all: conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM watcher_verbose WHERE name=?)",
            [name],
            |r| r.get(0),
        )?,
        filter: serde_json::from_str(&filter)?,
        kind,
        cursor,
        run_as,
    })
}

fn key(c: &Connection, a: &Access, name: &str) -> Result<String> {
    ensure!(
        !name.starts_with('@'),
        Missing.error("watcher_not_found", "unknown watcher")
    );
    if a.is_trusted() {
        return Ok(name.to_owned());
    }
    c.query_row(
        "SELECT internal FROM watcher_registry WHERE actor=? AND name=?",
        params![a.bound_actor(), name],
        |r| r.get(0),
    )
    .optional()?
    .context(Missing.error("watcher_not_found", "unknown watcher"))
}
struct Policies {
    caller: Access,
    owner: Option<Access>,
}
impl Policies {
    fn open(b: &Board, tx: &Transaction<'_>, name: &str) -> Result<(Watcher, Self)> {
        let caller = b.access_on(tx)?;
        ensure!(
            !caller.request_narrowed(),
            Forbidden.error(
                "scope_not_durable",
                "temporary scopes cannot consume or change a durable watcher"
            )
        );
        let w = get(tx, &key(tx, &caller, name)?)?;
        let id: Option<String> = tx
            .query_row(
                "SELECT credential FROM watcher_registry WHERE internal=?",
                [&w.key],
                |r| r.get(0),
            )
            .optional()?;
        let owner = if let Some(id) = id {
            Some(
                Board::credential_access_on(tx, &id)?.context(Forbidden.error(
                    "registration_revoked",
                    "watcher registration credential was revoked or expired",
                ))?,
            )
        } else {
            None
        };
        Ok((w, Self { caller, owner }))
    }
    fn registered(&self, c: &Connection, e: Event) -> Result<Option<Event>> {
        match &self.owner {
            Some(a) => crate::feed::allowed(c, a, e),
            None => Ok(Some(e)),
        }
    }
    fn delivered(&self, c: &Connection, e: Event) -> Result<Option<Event>> {
        match self.registered(c, e)? {
            Some(e) => crate::feed::allowed(c, &self.caller, e),
            None => Ok(None),
        }
    }
}
pub fn add_watcher(
    board: &Board,
    name: &str,
    filter: Filter,
    kind: Option<&str>,
) -> Result<Watcher> {
    add_watcher_as(board, "local", name, filter, kind)
}

pub fn add_watcher_as(
    board: &Board,
    actor: &str,
    name: &str,
    filter: Filter,
    kind: Option<&str>,
) -> Result<Watcher> {
    add_watcher_spec(
        board,
        actor,
        WatchSpec {
            name: name.into(),
            filter,
            kind: kind.map(str::to_owned),
            all: false,
        },
    )
}
pub fn add_watcher_spec(board: &Board, actor: &str, s: WatchSpec) -> Result<Watcher> {
    let tx = write(&board.conn)?;
    let a = board.access_on(&tx)?;
    let w = subscribe(&tx, &a, actor, &s.name, s.filter, s.kind.as_deref(), s.all)?;
    tx.commit()?;
    Ok(w)
}

fn validate_spec(
    a: &Access,
    actor: &str,
    name: &str,
    filter: &Filter,
    kind: Option<&str>,
) -> Result<()> {
    crate::validate_actor(actor)?;
    filter.validate()?;
    ensure!(
        name.as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_alphanumeric())
            && name.len() <= 64
            && name
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_'),
        "invalid watcher name"
    );
    ensure!(filter.after.is_none(), "watchers always start at the head");
    ensure!(
        filter.re.is_none_or(|id| id > 0),
        "watcher thread filter must be a positive message ID"
    );
    ensure!(
        !matches!(filter.mentions.as_deref(), Some("" | "me")),
        "a durable watcher has no me: give an explicit mention handle"
    );
    if let Some(channel) = &filter.channel {
        crate::validate_channel(channel.strip_suffix('/').unwrap_or(channel))?;
    }
    ensure!(
        kind.is_none_or(|s| !s.is_empty()
            && s.len() <= 128
            && s.bytes().all(|c| c.is_ascii_lowercase()
                || c.is_ascii_digit()
                || matches!(c, b'.' | b'_' | b'-'))),
        "invalid event kind"
    );
    a.actor(actor)?;
    ensure!(
        !a.request_narrowed(),
        Forbidden.error(
            "scope_not_durable",
            "cannot register a watcher from a temporarily narrowed request"
        )
    );
    ensure!(
        a.anywhere(Action::Read, filter.channel.as_deref()),
        Forbidden.error("missing_permission", "watcher scope is not readable")
    );
    Ok(())
}
fn subscribe(
    tx: &Transaction<'_>,
    a: &Access,
    actor: &str,
    name: &str,
    filter: Filter,
    kind: Option<&str>,
    all: bool,
) -> Result<Watcher> {
    validate_spec(a, actor, name, &filter, kind)?;
    let internal = if a.is_trusted() {
        name.to_owned()
    } else {
        format!("@{}", uuid::Uuid::new_v4())
    };
    Board::event_tx(
        tx,
        "watcher.created",
        actor,
        None,
        json!({"name":name,"filter":filter,"kind":kind,"all":all}),
    )?;
    let inserted = tx.execute(
        "INSERT INTO watchers(name,filter,kind,cursor) VALUES(?,?,?,(SELECT COALESCE(MAX(cursor),0) FROM events))
         ON CONFLICT(name) DO NOTHING",
        params![internal, serde_json::to_string(&filter)?, kind],
    )?;
    ensure!(
        inserted == 1,
        Conflict.error("watcher_conflict", "name is already subscribed")
    );
    let run_as = if let Some(id) = a.credential_id() {
        ensure!(
            tx.execute(
                "INSERT INTO watcher_registry VALUES(?,?,?,?) ON CONFLICT(actor,name) DO NOTHING",
                params![internal, actor, name, id]
            )? == 1,
            Conflict.error("watcher_conflict", "name is already subscribed")
        );
        actor.to_owned()
    } else {
        format!("{}/{}", actor.split('/').next().unwrap_or(actor), name)
    };
    tx.execute(
        "INSERT INTO watcher_scan(name,scanned,run_as) SELECT name,cursor,? FROM watchers WHERE name=?",
        params![run_as, internal],
    )?;
    if all {
        tx.execute("INSERT INTO watcher_verbose VALUES(?)", [&internal])?;
    }
    get(tx, &internal)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WatchSpec {
    pub name: String,
    #[serde(default)]
    pub filter: Filter,
    pub kind: Option<String>,
    #[serde(default)]
    pub all: bool,
}

/// Replace the calling actor's subscriptions as one configuration write. Unchanged
/// subscriptions keep their cursor, leases and queued matches. Empty means unsubscribe.
pub fn replace_watchers(b: &Board, actor: &str, specs: &[WatchSpec]) -> Result<Vec<Watcher>> {
    let tx = write(&b.conn)?;
    let a = b.access_on(&tx)?;
    a.actor(actor)?;
    ensure!(
        !a.request_narrowed(),
        Forbidden.error(
            "scope_not_durable",
            "temporary scopes cannot replace a registration"
        )
    );
    ensure!(
        a.anywhere(Action::Read, None),
        Forbidden.error("missing_permission", "watcher needs readable channels")
    );
    let mut names = std::collections::BTreeSet::new();
    for s in specs {
        validate_spec(&a, actor, &s.name, &s.filter, s.kind.as_deref())?;
        ensure!(names.insert(&s.name), "duplicate watcher name");
    }
    let rows=tx.prepare("SELECT w.name FROM watchers w JOIN watcher_scan s USING(name) LEFT JOIN watcher_registry r ON r.internal=w.name
       WHERE (?1 AND r.actor IS NULL AND (s.run_as=?2||'/'||w.name OR (s.run_as IS NULL AND ?2='local'))) OR (NOT ?1 AND r.actor=?3)")?
       .query_map(params![a.is_trusted(),actor.split('/').next().unwrap_or(actor),actor],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut existing = std::collections::BTreeMap::new();
    for internal in rows {
        let w = get(&tx, &internal)?;
        if !names.contains(&w.name) {
            tx.execute("DELETE FROM watchers WHERE name=?", [&internal])?;
            Board::event_tx(&tx, "watcher.removed", actor, None, json!({"name":w.name}))?;
        } else {
            existing.insert(w.name.clone(), w);
        }
    }
    let mut out = Vec::new();
    for s in specs {
        if let Some(old) = existing.remove(&s.name) {
            let credential: Option<String> = tx
                .query_row(
                    "SELECT credential FROM watcher_registry WHERE internal=?",
                    [&old.key],
                    |r| r.get(0),
                )
                .optional()?;
            let old_filter = serde_json::to_value(&old.filter)?;
            if old_filter != serde_json::to_value(&s.filter)?
                || old.kind != s.kind
                || old.all != s.all
                || credential.as_deref() != a.credential_id()
            {
                // Any event before reconfiguration is evaluated against the OLD scope/filter.
                let head = crate::read::cursor_on(&tx)?;
                if let Some(id) = credential.as_deref()
                    && Board::credential_access_on(&tx, id)?.is_none()
                {
                    tx.execute("DELETE FROM watcher_queue WHERE name=?", [&old.key])?;
                    tx.execute("DELETE FROM watcher_filter_hits WHERE name=?", [&old.key])?;
                    tx.execute(
                        "UPDATE watcher_scan SET scanned=? WHERE name=?",
                        params![head, old.key],
                    )?;
                    checkpoint(&tx, &old.key)?;
                } else {
                    let (_, p) = Policies::open(b, &tx, &s.name)?;
                    while scan(&tx, &old, &p)? {}
                }
                tx.execute(
                    "UPDATE watchers SET filter=?,kind=? WHERE name=?",
                    params![serde_json::to_string(&s.filter)?, s.kind, old.key],
                )?;
                if s.all {
                    tx.execute(
                        "INSERT OR IGNORE INTO watcher_verbose VALUES(?)",
                        [&old.key],
                    )?;
                } else {
                    tx.execute("DELETE FROM watcher_verbose WHERE name=?", [&old.key])?;
                }
                if let Some(id) = a.credential_id() {
                    tx.execute(
                        "UPDATE watcher_registry SET credential=? WHERE internal=?",
                        params![id, old.key],
                    )?;
                }
                Board::event_tx(
                    &tx,
                    "watcher.updated",
                    actor,
                    None,
                    json!({"name":s.name,"filter":s.filter,"kind":s.kind,"all":s.all}),
                )?;
            }
            out.push(get(&tx, &old.key)?);
        } else {
            out.push(subscribe(
                &tx,
                &a,
                actor,
                &s.name,
                s.filter.clone(),
                s.kind.as_deref(),
                s.all,
            )?);
        }
    }
    tx.commit()?;
    Ok(out)
}
pub fn get_watcher(board: &Board, name: &str) -> Result<Watcher> {
    let tx = board.conn.unchecked_transaction()?;
    let w = get(&tx, &key(&tx, &board.access_on(&tx)?, name)?)?;
    tx.commit()?;
    Ok(w)
}
pub fn watchers(board: &Board) -> Result<Vec<Watcher>> {
    Ok(registry_page(board, "", i64::MAX)?.1)
}
pub fn remove_watcher(board: &Board, name: &str) -> Result<()> {
    remove_watcher_as(board, "local", name)
}
pub fn remove_watcher_as(board: &Board, actor: &str, name: &str) -> Result<()> {
    crate::validate_actor(actor)?;
    let tx = write(&board.conn)?;
    let access = board.access_on(&tx)?;
    access.actor(actor)?;
    ensure!(
        !access.request_narrowed(),
        Forbidden.error(
            "scope_not_durable",
            "temporary scopes cannot remove a registration"
        )
    );
    let internal = key(&tx, &access, name)?;
    ensure!(
        tx.execute("DELETE FROM watchers WHERE name=?", [internal])? == 1,
        Missing.error("watcher_not_found", "unknown watcher")
    );
    Board::event_tx(&tx, "watcher.removed", actor, None, json!({"name":name}))?;
    tx.commit()?;
    Ok(())
}

pub fn watchers_page(board: &Board, after: &str, limit: usize) -> Result<(i64, Vec<Watcher>)> {
    ensure!((1..=501).contains(&limit), "invalid limit");
    registry_page(board, after, limit as i64)
}
fn registry_page(board: &Board, after: &str, limit: i64) -> Result<(i64, Vec<Watcher>)> {
    let tx = board.conn.unchecked_transaction()?;
    let access = board.access_on(&tx)?;
    let cursor = crate::read::cursor_on(&tx)?;
    let mut rows = Vec::new();
    for name in tx
        .prepare("SELECT w.name FROM watchers w LEFT JOIN watcher_registry r ON r.internal=w.name
            WHERE ((?1 AND r.actor IS NULL) OR r.actor=?2) AND coalesce(r.name,w.name)>?3 ORDER BY coalesce(r.name,w.name) LIMIT ?4")?
        .query_map(params![access.is_trusted(),access.bound_actor(),after,limit], |r| r.get::<_, String>(0))?
    {
        rows.push(get(&tx, &name?)?);
    }
    tx.commit()?;
    Ok((cursor, rows))
}

fn checkpoint(tx: &Transaction<'_>, name: &str) -> Result<()> {
    let through: i64 = tx.query_row(
        "SELECT min(s.scanned,coalesce((SELECT min(cursor)-1 FROM watcher_queue q
          WHERE q.name=s.name AND q.state='pending'),s.scanned)) FROM watcher_scan s WHERE s.name=?",
        [name],
        |r| r.get(0),
    )?;
    tx.execute(
        "UPDATE watchers SET cursor=? WHERE name=? AND cursor<?",
        params![through, name, through],
    )?;
    Ok(())
}

// Materialize ordered matches by reference to the immutable event log. This cursor can
// advance past a held lease: the queued row, not a replay of the tail, owns that delivery.
fn scan(tx: &Transaction<'_>, watcher: &Watcher, p: &Policies) -> Result<bool> {
    let scanned: i64 = tx.query_row(
        "SELECT scanned FROM watcher_scan WHERE name=?",
        [&watcher.key],
        |r| r.get(0),
    )?;
    let events = Board::events_on(tx, scanned, 128)?;
    let Some(last) = events.last().map(|e| e.cursor) else {
        return Ok(false);
    };
    let captured =
        p.owner.is_some() || watcher.filter.by.is_some() || watcher.filter.mentions.is_some();
    let since = if captured {
        tx.query_row(
            "SELECT through FROM watcher_filter_start WHERE singleton=1",
            [],
            |r| r.get(0),
        )?
    } else {
        i64::MAX
    };
    let hits: std::collections::HashSet<i64> = if captured && last > since {
        tx.prepare_cached(
            "SELECT cursor FROM watcher_filter_hits WHERE name=? AND cursor>? AND cursor<=?",
        )?
        .query_map(params![watcher.key, scanned, last], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?
    } else {
        Default::default()
    };
    let legacy = if captured && scanned < since {
        Some(crate::feed::EventMatcher::resolve(
            tx,
            p.owner.as_ref().unwrap_or(&p.caller),
            &watcher.filter,
        )?)
    } else {
        None
    };
    for event in events {
        let Some(event) = p.registered(tx, event)? else {
            continue;
        };
        let matched = if event.message.is_some() && event.cursor > since {
            hits.contains(&event.cursor)
        } else if let Some(legacy) = &legacy {
            legacy.matches(&event)
        } else {
            event.matches(&watcher.filter)
        };
        if !matched || watcher.kind.as_ref().is_some_and(|k| k != &event.kind) {
            continue;
        }
        if watcher.kind.is_none()
            && (event.message.is_none()
                || (!watcher.all
                    && matches!(
                        event.kind.as_str(),
                        "task.renewed" | "message.synced" | "message.files_synced"
                    )))
        {
            continue;
        }
        let scheduled = event.kind == "message.posted"
            && event.message.as_ref().is_some_and(|m| m.schedule.is_some());
        if watcher.run_as.as_ref() == Some(&event.actor) && !scheduled {
            continue;
        }
        let lane = event.message.as_ref().map_or(0, |m| m.re.unwrap_or(m.id));
        tx.execute(
            "INSERT OR IGNORE INTO watcher_queue(name,cursor,lane) VALUES(?,?,?)",
            params![watcher.key, event.cursor, lane],
        )?;
    }
    if !hits.is_empty() {
        tx.execute(
            "DELETE FROM watcher_filter_hits WHERE name=? AND cursor<=?",
            params![watcher.key, last],
        )?;
    }
    tx.execute(
        "UPDATE watcher_scan SET scanned=? WHERE name=?",
        params![last, watcher.key],
    )?;
    checkpoint(tx, &watcher.key)?;
    Ok(true)
}

fn expire(tx: &Transaction<'_>, name: &str, at: i64) -> Result<()> {
    let n = tx.execute(
        "UPDATE watcher_queue SET
            state=CASE WHEN attempts>=CASE WHEN EXISTS(SELECT 1 FROM watcher_registry WHERE internal=?1) THEN ?4 ELSE ?3 END THEN 'failed' ELSE 'pending' END,
            error='delivery lease expired',receipt=NULL,last_receipt=NULL,lease_until=NULL,offered=0
            WHERE name=?1 AND state='pending' AND lease_until<=?2",
        params![name, at, MAX_INTERRUPTED_ATTEMPTS,MAX_ATTEMPTS],
    )?;
    if n > 0 {
        checkpoint(tx, name)?;
    }
    Ok(())
}

fn available(tx: &Transaction<'_>, w: &Watcher, at: i64, p: &Policies) -> Result<Option<Event>> {
    let mut after = 0;
    loop {
        let found = tx.query_row(
            "SELECT q.cursor FROM watcher_queue q WHERE q.name=?1 AND q.state='pending'
            AND q.receipt IS NULL AND q.retry_at<=?2 AND q.cursor>?3 AND NOT EXISTS(SELECT 1 FROM watcher_queue prev
            WHERE prev.name=q.name AND prev.lane=q.lane AND prev.state='pending' AND prev.cursor<q.cursor)
            ORDER BY q.cursor LIMIT 1",
            params![w.key, at, after],
            |r| r.get(0),
        ).optional()?;
        if let Some(cursor) = found {
            after = cursor;
            let Some(e) = p.registered(tx, event_at(tx, cursor)?)? else {
                tx.execute("UPDATE watcher_queue SET state='skipped',receipt=NULL,last_receipt=NULL,lease_until=NULL,offered=0 WHERE name=? AND cursor=?",params![w.key,cursor])?;
                checkpoint(tx, &w.key)?;
                continue;
            };
            if let Some(e) = crate::feed::allowed(tx, &p.caller, e)? {
                return Ok(Some(e));
            }
        } else if !scan(tx, w, p)? {
            return Ok(None);
        }
    }
}

fn event_at(tx: &Transaction<'_>, cursor: i64) -> Result<Event> {
    Board::events_on(tx, cursor - 1, 1)?
        .into_iter()
        .next()
        .filter(|e| e.cursor == cursor)
        .context("missing queued board event")
}

/// Peek at the oldest available thread head. Its lane remains ordered until acknowledged.
pub fn next_event(board: &Board, name: &str) -> Result<Option<Event>> {
    let tx = write(&board.conn)?;
    let (w, policy) = Policies::open(board, &tx, name)?;
    let name = &w.key;
    let at = now();
    expire(&tx, name, at)?;
    let event = if let Some(event) = available(&tx, &w, at, &policy)? {
        tx.execute(
            "UPDATE watcher_queue SET offered=1 WHERE name=? AND cursor=? AND offered=0",
            params![name, event.cursor],
        )?;
        Some(event)
    } else {
        None
    };
    tx.commit()?;
    Ok(event)
}

/// Lease one thread's head. Other thread heads remain available to concurrent consumers.
pub fn lease_event(board: &Board, name: &str, lease_seconds: i64) -> Result<Option<Delivery>> {
    ensure!(
        (1..=3600).contains(&lease_seconds),
        "delivery lease must be 1..3600 seconds"
    );
    let tx = write(&board.conn)?;
    let (w, policy) = Policies::open(board, &tx, name)?;
    let d = lease_one(board, &tx, &w, &policy, lease_seconds)?;
    tx.commit()?;
    Ok(d)
}
fn lease_one(
    board: &Board,
    tx: &Transaction<'_>,
    w: &Watcher,
    policy: &Policies,
    lease_seconds: i64,
) -> Result<Option<Delivery>> {
    let name = &w.key;
    let at = now();
    expire(tx, name, at)?;
    let out = if let Some(event) = available(tx, w, at, policy)? {
        // A deadline stored in whole seconds must not truncate a one-second lease to milliseconds.
        let lease_until = at + lease_seconds + 1;
        let (receipt, attempts): (String,i64) = tx.query_row(
            "UPDATE watcher_queue SET receipt=lower(hex(randomblob(24))),
            lease_until=?, attempts=attempts+1,last_receipt=NULL, offered=0 WHERE name=? AND cursor=? RETURNING receipt,attempts",
            params![lease_until, name, event.cursor],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Some(Delivery {
            watcher: w.name.clone(),
            trigger: format!(
                "watch:{:x}",
                Sha256::digest(serde_json::to_vec(&(
                    super::claim_namespace(board)?,
                    name,
                    event.cursor
                ))?)
            ),
            event,
            receipt,
            lease_until,
            attempt: attempts - 1,
            attempts,
        })
    } else {
        None
    };
    Ok(out)
}

/// Lease a batch of independent thread heads across the caller's watch set.
pub fn lease_matches(b: &Board, limit: usize, seconds: i64) -> Result<Vec<Delivery>> {
    ensure!(
        (1..=100).contains(&limit) && (1..=3600).contains(&seconds),
        "limit must be 1..100 and lease_seconds 1..3600"
    );
    let tx = write(&b.conn)?;
    let a = b.access_on(&tx)?;
    ensure!(
        !a.request_narrowed(),
        Forbidden.error(
            "scope_not_durable",
            "temporary scopes cannot consume a durable watcher"
        )
    );
    let names=tx.prepare("SELECT coalesce(r.name,w.name) FROM watchers w LEFT JOIN watcher_registry r ON r.internal=w.name
        WHERE (?1 AND r.actor IS NULL) OR r.actor=?2 ORDER BY 1")?.query_map(params![a.is_trusted(),a.bound_actor()],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        !names.is_empty(),
        Missing.error("watcher_not_found", "no registered watchers")
    );
    let mut open = Vec::new();
    for n in names {
        match Policies::open(b, &tx, &n) {
            Ok(w) => open.push(w),
            Err(e) if crate::errors::classify(&e).1 == "registration_revoked" => (),
            Err(e) => return Err(e),
        }
    }
    let mut found = Vec::new();
    while found.len() < limit {
        let before = found.len();
        for (w, p) in &open {
            if let Some(d) = lease_one(b, &tx, w, p, seconds)? {
                found.push(d);
                if found.len() == limit {
                    break;
                }
            }
        }
        if found.len() == before {
            break;
        }
    }
    tx.commit()?;
    Ok(found)
}

pub fn ack_event(board: &Board, name: &str, cursor: i64) -> Result<()> {
    ack_delivery(board, name, cursor, None)
}
/// Extend an in-flight callback only while its proof and both read grants remain current.
pub fn renew_delivery(
    board: &Board,
    name: &str,
    cursor: i64,
    receipt: &str,
    seconds: i64,
) -> Result<i64> {
    ensure!(
        (1..=3600).contains(&seconds),
        "delivery lease must be 1..3600 seconds"
    );
    let tx = write(&board.conn)?;
    let (w, p) = Policies::open(board, &tx, name)?;
    let at = now();
    let until = at + seconds + 1;
    let count=tx.execute("UPDATE watcher_queue SET lease_until=? WHERE name=? AND cursor=? AND state='pending' AND receipt=? AND lease_until>?",
        params![until,w.key,cursor,receipt,at])?;
    ensure!(
        count == 1 && p.delivered(&tx, event_at(&tx, cursor)?)?.is_some(),
        Conflict.error(
            "delivery_conflict",
            "delivery was revoked or its lease ended"
        )
    );
    tx.commit()?;
    Ok(until)
}
pub fn ack_delivery(board: &Board, name: &str, cursor: i64, receipt: Option<&str>) -> Result<()> {
    settle_delivery(board, name, cursor, receipt, DeliveryOutcome::Ack)
}

struct QueueEntry {
    state: String,
    receipt: Option<String>,
    attempts: i64,
    offered: bool,
    last: Option<String>,
    error: Option<String>,
}

pub fn settle_delivery(
    board: &Board,
    name: &str,
    cursor: i64,
    receipt: Option<&str>,
    outcome: DeliveryOutcome<'_>,
) -> Result<()> {
    settle_delivery_if(board, name, cursor, receipt, outcome, false)
}

/// Strict settlement never treats a historical proof as a live lease.
pub fn settle_delivery_if(
    board: &Board,
    name: &str,
    cursor: i64,
    receipt: Option<&str>,
    outcome: DeliveryOutcome<'_>,
    if_pending: bool,
) -> Result<()> {
    ensure!(
        !if_pending || receipt.is_some_and(|s| !s.is_empty()),
        crate::errors::Kind::Invalid.error(
            "receipt_required",
            "if_pending requires a delivery lease receipt"
        )
    );
    if let DeliveryOutcome::Error(reason) = &outcome {
        ensure!(
            !reason.trim().is_empty() && reason.chars().count() <= 1000,
            "delivery error must be 1..1000 characters"
        );
    }
    let tx = write(&board.conn)?;
    let (w, policy) = Policies::open(board, &tx, name)?;
    let name = &w.key;
    let at = now();
    expire(&tx, name, at)?;
    let first = if if_pending {
        None
    } else {
        available(&tx, &w, at, &policy)?.map(|e| e.cursor)
    };
    let stale = || Conflict.error("delivery_conflict", "not the current delivery or lease");
    let row = tx
        .query_row(
            "SELECT state,receipt,attempts,offered,last_receipt,error
        FROM watcher_queue WHERE name=? AND cursor=?",
            params![name, cursor],
            |r| {
                Ok(QueueEntry {
                    state: r.get(0)?,
                    receipt: r.get(1)?,
                    attempts: r.get(2)?,
                    offered: r.get(3)?,
                    last: r.get(4)?,
                    error: r.get(5)?,
                })
            },
        )
        .optional()?
        .with_context(stale)?;
    ensure!(
        policy.delivered(&tx, event_at(&tx, cursor)?)?.is_some(),
        stale()
    );
    // A legacy expired/skipped row could retain a previous, failed attempt's receipt.
    let settled = match row.state.as_str() {
        "acked" => true,
        "skipped" => row.error.is_none(),
        "failed" => row.error.as_deref() != Some("delivery lease expired"),
        _ => false,
    };
    if if_pending && settled && row.last.as_deref() == receipt {
        return Err(Conflict
            .error("no_match", "this lease has already been settled")
            .into());
    }
    let succeeded = match &outcome {
        DeliveryOutcome::Ack => row.state == "acked",
        DeliveryOutcome::Skip => row.state == "skipped",
        DeliveryOutcome::Error(reason) => {
            receipt.is_some() && row.error.as_deref() == Some(*reason)
        }
    };
    if !if_pending && succeeded && row.last.as_deref() == receipt {
        tx.commit()?;
        return Ok(());
    }
    let valid = match receipt {
        Some(proof) => row.receipt.as_deref() == Some(proof),
        None => row.receipt.is_none() && (row.offered || first == Some(cursor)),
    };
    ensure!(row.state == "pending" && valid, stale());
    let (state, attempts, retry_at, error) = match outcome {
        DeliveryOutcome::Ack => ("acked", row.attempts, 0, None),
        DeliveryOutcome::Skip => ("skipped", row.attempts, 0, None),
        DeliveryOutcome::Error(reason) => {
            let count = row.attempts + i64::from(receipt.is_none());
            if count >= MAX_ATTEMPTS {
                ("failed", count, 0, Some(reason))
            } else {
                (
                    "pending",
                    count,
                    at + (1i64 << (count - 1)).min(30),
                    Some(reason),
                )
            }
        }
    };
    tx.execute(
        "UPDATE watcher_queue SET state=?,attempts=?,retry_at=?,last_receipt=?,error=?,
        receipt=NULL,lease_until=NULL,offered=0 WHERE name=? AND cursor=?",
        params![state, attempts, retry_at, receipt, error, name, cursor],
    )?;
    checkpoint(&tx, name)?;
    tx.commit()?;
    Ok(())
}

/// Failed thread heads are kept for inspection without stopping later events in the lane.
pub fn watcher_failures(
    board: &Board,
    name: &str,
    after: i64,
    limit: usize,
) -> Result<(i64, Vec<WatchFailure>)> {
    ensure!(
        after >= 0 && (1..=501).contains(&limit),
        "invalid cursor or limit"
    );
    let tx = write(&board.conn)?;
    let (w, policy) = Policies::open(board, &tx, name)?;
    let name = &w.key;
    expire(&tx, name, now())?;
    let head = crate::read::cursor_on(&tx)?;
    let mut stmt = tx.prepare(
        "SELECT cursor,nullif(lane,0),attempts,error FROM watcher_queue
        WHERE name=? AND cursor>? AND state='failed' ORDER BY cursor",
    )?;
    let mut rows = Vec::new();
    for row in stmt.query_map(params![name, after], |r| {
        Ok(WatchFailure {
            cursor: r.get(0)?,
            thread_id: r.get(1)?,
            attempts: r.get(2)?,
            error: r.get(3)?,
        })
    })? {
        let row = row?;
        if policy.delivered(&tx, event_at(&tx, row.cursor)?)?.is_some() {
            rows.push(row);
            if rows.len() == limit {
                break;
            }
        }
    }
    drop(stmt);
    tx.commit()?;
    Ok((head, rows))
}
