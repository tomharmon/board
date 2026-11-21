use crate::{
    Board, Event, Filter,
    access::{Access, Action},
};
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

pub(crate) fn allowed(c: &Connection, a: &Access, mut event: Event) -> Result<Option<Event>> {
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
        ensure!(
            after >= 0 && (1..=500).contains(&limit),
            "invalid cursor or limit"
        );
        self.visible_events(after, limit, filter, kind)
    }
    pub(crate) fn visible_events(
        &self,
        after: i64,
        limit: usize,
        filter: &Filter,
        kind: Option<&str>,
    ) -> Result<(i64, Vec<Event>)> {
        let tx = self.conn.unchecked_transaction()?;
        let a = self.access_on(&tx)?;
        let (authors, filters) = expanded(&tx, &a, filter)?;
        let mut pos = after;
        let mut found = Vec::new();
        'scan: while found.len() < limit {
            let batch = Board::events_on(&tx, pos, 128)?;
            if batch.is_empty() {
                break;
            }
            for event in batch {
                pos = event.cursor;
                if kind.is_none_or(|k| k == event.kind)
                    && authors.as_ref().is_none_or(|by| {
                        event.message.as_ref().is_some_and(|m| by.matches(&m.actor))
                    })
                    && filters.iter().any(|f| event.matches(f))
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
        ensure!(count <= 100, "tail history cannot exceed 100 events");
        let tx = self.conn.unchecked_transaction()?;
        let a = self.access_on(&tx)?;
        let (authors, filters) = expanded(&tx, &a, filter)?;
        let head: i64 = tx.query_row("SELECT coalesce(max(cursor),0) FROM events", [], |r| {
            r.get(0)
        })?;
        let mut end = head;
        let mut found = Vec::new();
        while end > 0 && found.len() < count {
            let start = end.saturating_sub(128).max(0);
            let rows = Board::events_on(&tx, start, (end - start) as usize)?;
            for event in rows.into_iter().rev().filter(|e| {
                e.cursor <= end
                    && authors
                        .as_ref()
                        .is_none_or(|by| e.message.as_ref().is_some_and(|m| by.matches(&m.actor)))
                    && filters.iter().any(|f| e.matches(f))
            }) {
                if let Some(e) = allowed(&tx, &a, event)? {
                    found.push(e);
                    if found.len() == count {
                        break;
                    }
                }
            }
            end = start;
        }
        found.reverse();
        let mut out = Vec::new();
        for mut event in found {
            if event.kind == "source.delivery_failed"
                && let (Some(source), Some(item)) = (
                    event.payload.get("source").and_then(|v| v.as_str()),
                    event.payload.get("item").and_then(|v| v.as_i64()),
                )
            {
                let healed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sync_queue WHERE id=? AND source=? AND state IN ('done','skipped'))",params![item,source],|r|r.get(0))?;
                if healed {
                    continue;
                }
            }
            if let Some(snapshot) = event.message.take() {
                let current = a.view(&tx, Board::get_on(&tx, snapshot.id)?)?;
                if (filter.text.is_some() || filter.mentions.is_some())
                    && (current.deleted
                        || current.text != snapshot.text
                        || filter.text.as_ref().is_some_and(|q| {
                            !current.text.to_lowercase().contains(&q.to_lowercase())
                        })
                        || (filter.mentions.is_some()
                            && !crate::core::mentions(&current.text).any(|term| {
                                filters
                                    .iter()
                                    .filter_map(|f| f.mentions.as_deref())
                                    .any(|q| crate::prose::matches(&term, q))
                            })))
                {
                    continue;
                }
                match event.kind.as_str() {
                    "labels.changed" => {
                        if let Some(set) =
                            event.payload.get_mut("set").and_then(|s| s.as_object_mut())
                        {
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
                        continue;
                    }
                    "task.failed" | "task.blocked"
                        if snapshot.task.as_ref().map(|t| (&t.state, &t.result))
                            != current.task.as_ref().map(|t| (&t.state, &t.result)) =>
                    {
                        continue;
                    }
                    _ => {}
                }
                if event.payload.get("task").is_some() {
                    event.payload["task"] = serde_json::to_value(&current.task)?;
                }
                event.message = Some(current);
            }
            out.push(event);
        }
        tx.commit()?;
        Ok((head, out))
    }
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

fn expanded(
    c: &Connection,
    a: &Access,
    f: &Filter,
) -> Result<(Option<crate::actors::Authors>, Vec<Filter>)> {
    f.validate()?;
    let mut f = f.clone();
    let authors =
        f.by.take()
            .as_deref()
            .map(|q| crate::actors::author_filter(c, a, q))
            .transpose()?;
    let filters = match &f.mentions {
        None => vec![f],
        Some(q) => crate::actors::mention_terms(c, a, q)?
            .into_iter()
            .map(|term| Filter {
                mentions: Some(term),
                ..f.clone()
            })
            .collect(),
    };
    Ok((authors, filters))
}
