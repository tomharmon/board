use super::{now, write};
use crate::access::{Access, Action};
use crate::errors::Kind::{Conflict, Forbidden, Missing};
use crate::{Board, Event, Filter};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};

#[derive(Debug, Serialize)]
pub struct Watcher {
    pub name: String,
    #[serde(skip)]
    key: String,
    pub filter: Filter,
    pub kind: Option<String>,
    pub cursor: i64,
    /// Writes made as this watcher's profile don't feed back into it, except scheduled posts.
    pub run_as: Option<String>,
}

/// A pull consumer can lease a delivery instead of peeking. Only this receipt can ack the lease.
#[derive(Debug, Serialize)]
pub struct Delivery {
    pub event: Event,
    /// Stable across attempts and unique to the board, registration and event.
    pub trigger: String,
    pub receipt: String,
    pub lease_until: i64,
    pub attempt: i64,
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
        CREATE INDEX IF NOT EXISTS watcher_lane ON watcher_queue(name,lane,cursor) WHERE state='pending';
        CREATE INDEX IF NOT EXISTS watcher_pending ON watcher_queue(name,cursor) WHERE state='pending';
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
    crate::validate_actor(actor)?;
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
    let tx = write(&board.conn)?;
    let a = board.access_on(&tx)?;
    a.actor(actor)?;
    ensure!(
        !a.request_narrowed(),
        Forbidden.error(
            "scope_not_durable",
            "cannot register a watcher from a temporarily narrowed request"
        )
    );
    if let Some(scope) = &filter.channel {
        let scope = scope.strip_suffix('/').unwrap_or(scope);
        ensure!(
            a.allows(Action::Read, scope)
                || filter.channel.as_deref().is_some_and(|s| s.ends_with('/'))
                    && a.channels(&tx, Action::Read)?.is_some_and(|paths| paths
                        .iter()
                        .any(|p| p.starts_with(&format!("{scope}/")))),
            Forbidden.error("missing_permission", "watcher scope is not readable")
        );
    } else {
        ensure!(
            a.channels(&tx, Action::Read)?.is_none_or(|v| !v.is_empty()),
            Forbidden.error(
                "missing_permission",
                "watcher needs at least one readable channel"
            )
        );
    }
    let internal = if a.is_trusted() {
        name.to_owned()
    } else {
        format!("@{}", uuid::Uuid::new_v4())
    };
    Board::event_tx(
        &tx,
        "watcher.created",
        actor,
        None,
        json!({"name":name,"filter":filter,"kind":kind}),
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
    let w = get(&tx, &internal)?;
    tx.commit()?;
    Ok(w)
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
    for event in events {
        let Some(event) = p.registered(tx, event)? else {
            continue;
        };
        if watcher.kind.as_ref().is_some_and(|k| k != &event.kind)
            || !event.matches(&watcher.filter)
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
            state=CASE WHEN attempts>=?3 THEN 'failed' ELSE 'pending' END,
            error='delivery lease expired',receipt=NULL,lease_until=NULL,offered=0
            WHERE name=?1 AND state='pending' AND lease_until<=?2",
        params![name, at, MAX_INTERRUPTED_ATTEMPTS],
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
                tx.execute("UPDATE watcher_queue SET state='skipped',receipt=NULL,lease_until=NULL,offered=0 WHERE name=? AND cursor=?",params![w.key,cursor])?;
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
    let name = &w.key;
    let at = now();
    expire(&tx, name, at)?;
    let out = if let Some(event) = available(&tx, &w, at, &policy)? {
        // A deadline stored in whole seconds must not truncate a one-second lease to milliseconds.
        let lease_until = at + lease_seconds + 1;
        let (receipt, attempt) = tx.query_row(
            "UPDATE watcher_queue SET receipt=lower(hex(randomblob(24))),
            lease_until=?, attempts=attempts+1, offered=0 WHERE name=? AND cursor=? RETURNING receipt,attempts",
            params![lease_until, name, event.cursor],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Some(Delivery {
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
            attempt,
        })
    } else {
        None
    };
    tx.commit()?;
    Ok(out)
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
    let first = available(&tx, &w, at, &policy)?.map(|e| e.cursor);
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
        .with_context(|| Conflict.error("delivery_conflict", "not a pending delivery"))?;
    ensure!(
        policy.delivered(&tx, event_at(&tx, cursor)?)?.is_some(),
        Conflict.error("delivery_conflict", "not a pending delivery")
    );
    let succeeded = match &outcome {
        DeliveryOutcome::Ack => row.state == "acked",
        DeliveryOutcome::Skip => row.state == "skipped",
        DeliveryOutcome::Error(reason) => {
            receipt.is_some() && row.error.as_deref() == Some(*reason)
        }
    };
    if succeeded && row.last.as_deref() == receipt {
        tx.commit()?;
        return Ok(());
    }
    let valid = match receipt {
        Some(proof) => row.receipt.as_deref() == Some(proof),
        None => row.receipt.is_none() && (row.offered || first == Some(cursor)),
    };
    ensure!(
        row.state == "pending" && valid,
        Conflict.error("delivery_conflict", "not the current delivery or lease")
    );
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
