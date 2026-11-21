use crate::{
    Board, Event, Filter,
    access::{Access, Action},
};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::{HashMap, hash_map::Entry},
    rc::Rc,
};

fn normal(event: &Event) -> bool {
    event.message.is_some()
        && !matches!(
            event.kind.as_str(),
            "actor.updated"
                | "actor.seen"
                | "source.receipt"
                | "message.synced"
                | "message.files_synced"
                | "task.renewed"
        )
}

pub(crate) fn allowed(c: &Connection, a: &Access, mut event: Event) -> Result<Option<Event>> {
    // Direct messages belong exclusively to the actor-scoped swarm feed.
    if event.kind == "swarm.dm" {
        return Ok(None);
    }
    let source_control = event.kind.starts_with("source.")
        || matches!(
            event.kind.as_str(),
            "thread.sync_requested" | "message.linked" | "thread.linked"
        );
    if source_control && !a.is_trusted() {
        let Some(source) = event.payload.get("source").and_then(|s| s.as_str()) else {
            return Ok(None);
        };
        let info = c
            .query_row(
                "SELECT owner,channel FROM sync_sources WHERE name=?",
                [source],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;
        if a.integration_narrowed()
            || !info.is_some_and(|(who, ch)| {
                a.bound_actor()
                    .is_some_and(|actor| crate::owned_by(&who, actor))
                    && a.allows(Action::Read, &ch)
                    && a.allows(Action::Sync, &ch)
            })
        {
            return Ok(None);
        }
    }
    if let Some(m) = event.message.take() {
        if !a.allows(Action::Read, &m.channel) {
            return Ok(None);
        }
        let m = a.view(c, m)?;
        if !a.everywhere(Action::Read) && event.payload.get("task").is_some() {
            event.payload["task"] = serde_json::to_value(&m.task)?;
        }
        event.message = Some(m);
    } else if !a.everywhere(Action::Read)
        || (!a.is_trusted()
            && [
                "grant.",
                "rule.",
                "credential.",
                "watcher.",
                "schedule.",
                "upload.",
                "attachment.",
            ]
            .iter()
            .any(|s| event.kind.starts_with(s)))
    {
        return Ok(None);
    }
    Ok(Some(event))
}

impl Board {
    /// Reapply current grants to a buffered event before sending it to a subscriber.
    pub fn recheck_event(&self, event: Event) -> Result<Option<Event>> {
        let tx = self.conn.unchecked_transaction()?;
        let projected = allowed(&tx, &self.access_on(&tx)?, event)?;
        tx.commit()?;
        Ok(projected)
    }
    /// Scan a single snapshot. On a sparse feed the cursor still advances past
    /// nonmatches; on a full page it stops at the last delivery, never the live head.
    pub fn event_page(
        &self,
        after: i64,
        limit: usize,
        filter: &Filter,
        kind: Option<&str>,
    ) -> Result<(i64, Vec<Event>)> {
        self.event_feed(after, limit, filter, kind, true)
    }
    /// The ordinary feed describes changes to messages; opt in to bookkeeping.
    /// An explicitly selected kind is also an opt-in. Neither bypasses current grants.
    pub fn event_feed(
        &self,
        after: i64,
        limit: usize,
        filter: &Filter,
        kind: Option<&str>,
        all: bool,
    ) -> Result<(i64, Vec<Event>)> {
        ensure!(
            after >= 0 && (1..=500).contains(&limit),
            "invalid cursor or limit"
        );
        self.scan_events(after, limit, filter, kind, all || kind.is_some())
    }
    pub(crate) fn visible_events(
        &self,
        after: i64,
        limit: usize,
        filter: &Filter,
        kind: Option<&str>,
    ) -> Result<(i64, Vec<Event>)> {
        self.scan_events(after, limit, filter, kind, true)
    }
    fn scan_events(
        &self,
        after: i64,
        limit: usize,
        filter: &Filter,
        kind: Option<&str>,
        all: bool,
    ) -> Result<(i64, Vec<Event>)> {
        let tx = self.conn.unchecked_transaction()?;
        let a = self.access_on(&tx)?;
        let matcher = EventMatcher::compile(&tx, &a, filter)?;
        let mut pos = after.min(crate::read::cursor_on(&tx)?);
        let mut found = Vec::new();
        'scan: while found.len() < limit {
            let batch = Board::events_on(&tx, pos, 128)?;
            if batch.is_empty() {
                break;
            }
            for event in batch {
                pos = event.cursor;
                if (all || normal(&event))
                    && kind.is_none_or(|k| k == event.kind)
                    && matcher.matches(&event)
                    && let Some(event) = allowed(&tx, &a, event)?
                {
                    found.push(event);
                    if found.len() == limit {
                        break 'scan;
                    }
                }
            }
        }
        tx.commit()?;
        Ok((pos, found))
    }

    /// Seed a tail with its last N *matching* events, not just matches among the last N writes.
    pub fn event_tail(&self, count: usize, filter: &Filter) -> Result<(i64, Vec<Event>)> {
        self.tail_feed(count, filter, true)
    }
    pub fn tail_feed(&self, count: usize, filter: &Filter, all: bool) -> Result<(i64, Vec<Event>)> {
        ensure!(count <= 100, "tail history cannot exceed 100 events");
        let tx = self.conn.unchecked_transaction()?;
        let a = self.access_on(&tx)?;
        let matcher = EventMatcher::compile(&tx, &a, filter)?;
        let head: i64 = tx.query_row("SELECT coalesce(max(cursor),0) FROM events", [], |r| {
            r.get(0)
        })?;
        let mut end = head;
        let mut found = Vec::new();
        while end > 0 && found.len() < count {
            let start = end.saturating_sub(128).max(0);
            let rows = Board::events_on(&tx, start, (end - start) as usize)?;
            for event in rows
                .into_iter()
                .rev()
                .filter(|e| e.cursor <= end && (all || normal(e)) && matcher.matches(e))
            {
                if let Some(e) = allowed(&tx, &a, event)?
                    && let Some(e) = current_event(&tx, &a, e, filter, &matcher.filters)?
                {
                    found.push(e);
                    if found.len() == count {
                        break;
                    }
                }
            }
            end = start;
        }
        found.reverse();
        tx.commit()?;
        Ok((head, found))
    }
}
fn current_event(
    tx: &Connection,
    a: &Access,
    mut event: Event,
    filter: &Filter,
    filters: &[Filter],
) -> Result<Option<Event>> {
    if event.kind == "source.delivery_failed"
        && let (Some(source), Some(item)) = (
            event.payload.get("source").and_then(|v| v.as_str()),
            event.payload.get("item").and_then(|v| v.as_i64()),
        )
    {
        let healed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sync_queue WHERE id=? AND source=? AND state IN ('done','skipped'))",params![item,source],|r|r.get(0))?;
        if healed {
            return Ok(None);
        }
    }
    if let Some(snapshot) = event.message.take() {
        let current = a.view(tx, Board::get_on(tx, snapshot.id)?)?;
        if (filter.text.is_some() || filter.mentions.is_some())
            && (current.deleted
                || current.text != snapshot.text
                || filter
                    .text
                    .as_ref()
                    .is_some_and(|q| !current.text.to_lowercase().contains(&q.to_lowercase()))
                || (filter.mentions.is_some()
                    && !crate::core::mentions(&current.text).any(|term| {
                        filters
                            .iter()
                            .filter_map(|f| f.mentions.as_deref())
                            .any(|q| crate::prose::matches(&term, q))
                    })))
        {
            return Ok(None);
        }
        match event.kind.as_str() {
            "source.receipt" | "message.synced" | "message.files_synced" if current.deleted => {
                return Ok(None);
            }
            "labels.changed" => {
                if let Some(set) = event.payload.get_mut("set").and_then(|s| s.as_object_mut()) {
                    set.retain(|k, v| {
                        v.as_str()
                            .is_some_and(|s| current.labels.get(k).is_some_and(|v| v == s))
                    });
                }
            }
            "reaction.added" | "reaction.removed"
                if !event
                    .payload
                    .get("emoji")
                    .and_then(|v| v.as_str())
                    .and_then(|k| current.reactions.get(k))
                    .is_some_and(|actors| actors.contains(&event.actor)) =>
            {
                return Ok(None);
            }
            "task.failed" | "task.blocked"
                if snapshot.task.as_ref().map(|t| (&t.state, &t.result))
                    != current.task.as_ref().map(|t| (&t.state, &t.result)) =>
            {
                return Ok(None);
            }
            _ => {}
        }
        if event.payload.get("task").is_some() {
            event.payload["task"] = serde_json::to_value(&current.task)?;
        }
        event.message = Some(current);
    }
    Ok(Some(event))
}
impl Board {
    pub fn activity_seen(&self, actor: &str) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        let key = self.access_on(&tx)?.private_key(actor)?;
        let seen = tx.query_row(
            "SELECT coalesce((SELECT cursor FROM activity_seen WHERE actor=?),0)",
            [key],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(seen)
    }
    pub fn mark_activity_seen(&self, actor: &str, through: i64) -> Result<()> {
        crate::validate_actor(actor)?;
        self.write(|tx|{let key=self.access_on(tx)?.private_key(actor)?;ensure!((0..=crate::read::cursor_on(tx)?).contains(&through),"activity position is outside the board");
            tx.execute("INSERT INTO activity_seen VALUES(?,?) ON CONFLICT(actor) DO UPDATE SET cursor=max(cursor,excluded.cursor)",rusqlite::params![key,through])?;Ok(())})
    }
    pub fn activity_unread(&self, actor: &str) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        let a = self.access_on(&tx)?;
        let key = a.private_key(actor)?;
        let after: i64 = tx.query_row(
            "SELECT coalesce((SELECT cursor FROM activity_seen WHERE actor=?),0)",
            [key],
            |r| r.get(0),
        )?;
        let mut pos = after;
        let mut count = 0;
        if a.is_trusted() {
            let count = tx.query_row(
                "SELECT count(*) FROM events WHERE cursor>? AND actor<>?",
                rusqlite::params![after, actor],
                |r| r.get(0),
            )?;
            tx.commit()?;
            return Ok(count);
        }
        loop {
            let rows = Self::events_on(&tx, pos, 256)?;
            if rows.is_empty() {
                break;
            }
            for e in rows {
                pos = e.cursor;
                if e.actor != actor && allowed(&tx, &a, e)?.is_some() {
                    count += 1;
                }
            }
        }
        tx.commit()?;
        Ok(count)
    }
}

pub(crate) struct EventMatcher {
    authors: Option<Rc<crate::actors::Authors>>,
    filters: Vec<Filter>,
}

/// Resolves directory-sensitive terms within one read snapshot and one audience.
pub(crate) struct EventResolver<'a> {
    conn: &'a Connection,
    access: &'a Access,
    by: HashMap<String, Rc<crate::actors::Authors>>,
    mentions: HashMap<String, Vec<String>>,
}

fn lookup<T>(
    entries: &mut HashMap<String, T>,
    query: String,
    resolve: impl FnOnce(&str) -> Result<T>,
) -> Result<&T> {
    Ok(match entries.entry(query) {
        Entry::Occupied(existing) => existing.into_mut(),
        Entry::Vacant(vacant) => {
            let value = resolve(vacant.key())?;
            vacant.insert(value)
        }
    })
}

impl<'a> EventResolver<'a> {
    pub(crate) fn new(conn: &'a Connection, access: &'a Access) -> Self {
        Self {
            conn,
            access,
            by: HashMap::new(),
            mentions: HashMap::new(),
        }
    }
    pub(crate) fn resolve(&mut self, filter: &Filter) -> Result<EventMatcher> {
        let mut f = filter.clone();
        let authors =
            f.by.take()
                .map(|q| {
                    lookup(&mut self.by, q, |q| {
                        crate::actors::author_filter(self.conn, self.access, q).map(Rc::new)
                    })
                    .cloned()
                })
                .transpose()?;
        let filters = match f.mentions.take() {
            None => vec![f],
            Some(q) => lookup(&mut self.mentions, q, |q| {
                crate::actors::mention_terms(self.conn, self.access, q)
            })?
            .iter()
            .map(|term| Filter {
                mentions: Some(term.clone()),
                ..f.clone()
            })
            .collect(),
        };
        Ok(EventMatcher { authors, filters })
    }
}

impl EventMatcher {
    pub(crate) fn compile(c: &Connection, a: &Access, f: &Filter) -> Result<Self> {
        f.validate()?;
        Self::resolve(c, a, f)
    }
    pub(crate) fn resolve(c: &Connection, a: &Access, f: &Filter) -> Result<Self> {
        EventResolver::new(c, a).resolve(f)
    }
    pub(crate) fn matches(&self, event: &Event) -> bool {
        self.authors
            .as_ref()
            .is_none_or(|by| event.message.as_ref().is_some_and(|m| by.matches(&m.actor)))
            && self.filters.iter().any(|f| event.matches(f))
    }
}
