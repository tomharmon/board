use crate::{
    Board, Message,
    access::{Access, Action},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params, params_from_iter, types::Value as Bind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Default, Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Filter {
    pub channel: Option<String>,
    pub from: Option<String>,
    pub by: Option<String>,
    pub text: Option<String>,
    pub labels: BTreeMap<String, String>,
    /// Structured query terms distinguish "key" (present) from "key=" (exactly empty).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub label_terms: Vec<LabelTerm>,
    pub after: Option<i64>,
    pub re: Option<i64>,
    pub mentions: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LabelTerm {
    pub key: String,
    pub values: Option<Vec<String>>,
}
impl LabelTerm {
    pub fn parse(raw: &[String]) -> Result<Vec<Self>> {
        ensure!(raw.len() <= 32, "at most 32 label filters");
        raw.iter()
            .map(|term| {
                let (k, v) = term
                    .split_once('=')
                    .map_or((term.as_str(), None), |(k, v)| (k, Some(v)));
                crate::core::validate_label_key(k)?;
                ensure!(
                    v.is_none_or(|s| s.len() <= 2048),
                    "label filter is too long"
                );
                Ok(Self {
                    key: k.into(),
                    values: v.map(|v| v.split('|').map(str::to_string).collect()),
                })
            })
            .collect()
    }
    fn matches(&self, v: Option<&String>) -> bool {
        v.is_some_and(|v| {
            self.values
                .as_ref()
                .is_none_or(|options| options.contains(v))
        })
    }
}
impl Filter {
    /// Resolve viewer-relative selectors before persisting or reusing a filter.
    /// An exact actor is distinct from a person (who also covers their profiles).
    pub fn bind_actor(&mut self, actor: &str) {
        if self.by.as_deref() == Some("me") {
            self.by = Some(format!("@[{actor}]"));
        }
        if matches!(self.mentions.as_deref(), Some("" | "me")) {
            self.mentions = Some(format!("@[{actor}]"));
        }
    }
    pub fn validate(&self) -> Result<()> {
        if let Some(channel) = &self.channel {
            crate::validate_channel(channel.strip_suffix('/').unwrap_or(channel))?;
        }
        ensure!(
            self.after.is_none_or(|x| x >= 0) && self.re.is_none_or(|x| x > 0),
            "invalid filter cursor or thread ID"
        );
        ensure!(
            self.text.as_ref().is_none_or(|s| !s.is_empty()
                && s.encode_utf16().count() <= 200
                && !s.contains('\0')),
            "text filter must contain 1-200 characters"
        );
        ensure!(
            self.from.as_ref().is_none_or(|s| !s.is_empty()
                && s.encode_utf16().count() <= 165
                && !s.chars().any(char::is_control)),
            "invalid source filter"
        );
        ensure!(
            self.labels.len() + self.label_terms.len() <= 32,
            "at most 32 label filters"
        );
        for k in self
            .labels
            .keys()
            .chain(self.label_terms.iter().map(|t| &t.key))
        {
            crate::core::validate_label_key(k)?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub name: String,
    pub threads: i64,
    pub messages: i64,
    pub last: Option<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadSummary {
    pub message: Message,
    pub replies: i64,
    pub last: Option<Message>,
}

#[derive(Debug, Serialize)]
pub struct Conversation {
    pub message: Message,
    pub replies: Vec<Message>,
    pub schedules: Vec<crate::automation::Schedule>,
    pub cursor: i64,
}

/// One SQLite read snapshot. Resume a forward scan at `cursor` or page back using `older`.
#[derive(Debug, Serialize)]
pub struct Window<T> {
    pub rows: Vec<T>,
    pub snapshot: i64,
    pub cursor: i64,
    pub older: Option<i64>,
}

fn check_window(after: Option<i64>, before: Option<i64>, limit: usize) -> Result<()> {
    ensure!((1..=500).contains(&limit), "limit must be 1..500");
    ensure!(
        after.is_none() || before.is_none(),
        "after and before cannot be combined"
    );
    ensure!(
        after.is_none_or(|n| n >= 0) && before.is_none_or(|n| n > 0),
        "invalid page cursor"
    );
    Ok(())
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub cursor: i64,
    #[serde(rename = "type")]
    pub kind: String,
    pub actor: String,
    pub message: Option<Message>,
    pub created_at: String,
    pub payload: serde_json::Value,
    #[serde(skip)]
    previous: Option<Message>,
    #[serde(skip)]
    root_labels: BTreeMap<String, String>,
}

fn channel_match(f: &str, s: &str) -> bool {
    if let Some(base) = f.strip_suffix('/') {
        s.starts_with(f) || s == base
    } else {
        s == f
    }
}
pub(crate) fn label_match(f: &str, actual: Option<&String>) -> bool {
    actual.is_some_and(|s| f.is_empty() || f.split('|').any(|p| p == s))
}
impl Event {
    pub fn matches(&self, f: &Filter) -> bool {
        if f.after.is_some_and(|a| self.cursor <= a) {
            return false;
        }
        let matches = |m: &Message| {
            f.channel
                .as_ref()
                .is_none_or(|c| channel_match(c, &m.channel))
                && f.from.as_ref().is_none_or(|s| {
                    s == m
                        .from
                        .as_deref()
                        .or_else(|| m.origin.as_ref().map(|o| o.source.as_str()))
                        .unwrap_or("board")
                })
                && f.by.as_ref().is_none_or(|who| {
                    if let Some(id) = crate::prose::exact_selector(who) {
                        return m.actor == id;
                    }
                    let who = who.trim_start_matches('@');
                    m.actor == who
                        || m.actor
                            .split_once('/')
                            .is_some_and(|(person, _)| person == who)
                })
                && f.re.is_none_or(|root| m.re.unwrap_or(m.id) == root)
                && f.text
                    .as_ref()
                    .is_none_or(|t| m.text.to_lowercase().contains(&t.to_lowercase()))
                && f.mentions.as_ref().is_none_or(|who| {
                    crate::core::mentions(&m.text).any(|h| crate::prose::matches(&h, who))
                })
                && f.label_terms.iter().all(|t| {
                    t.matches(m.labels.get(&t.key)) || t.matches(self.root_labels.get(&t.key))
                })
                && f.labels.iter().all(|(k, v)| {
                    label_match(v, m.labels.get(k)) || label_match(v, self.root_labels.get(k))
                })
        };
        self.message.as_ref().is_some_and(matches)
            || self.previous.as_ref().is_some_and(matches)
            || (self.message.is_none()
                && f.channel.is_none()
                && f.from.is_none()
                && f.by.is_none()
                && f.re.is_none()
                && f.text.is_none()
                && f.labels.is_empty()
                && f.label_terms.is_empty()
                && f.mentions.is_none())
    }
}

// Only static fragments are assembled; all user input is bound as SQL values.
pub(crate) fn predicate(f: &Filter) -> (String, Vec<Bind>) {
    let mut sql = String::from("1=1");
    let mut bind = Vec::new();
    if let Some(c) = &f.channel {
        if let Some(base) = c.strip_suffix('/') {
            sql.push_str(" AND (m.channel=? OR instr(m.channel,?)=1)");
            bind.push(base.to_owned().into());
        } else {
            sql.push_str(" AND m.channel=?");
        }
        bind.push(c.clone().into());
    }
    if let Some(t) = &f.by {
        if let Some(exact) = crate::prose::exact_selector(t) {
            sql.push_str(" AND m.actor=?");
            bind.push(exact.to_owned().into());
        } else {
            let who = t.trim_start_matches('@');
            sql.push_str(" AND (m.actor=? OR instr(m.actor,?)=1)");
            bind.push(who.to_owned().into());
            bind.push(format!("{who}/").into());
        }
    }
    if let Some(source) = &f.from {
        if source == "board" {
            sql.push_str(
                " AND NOT EXISTS(SELECT 1 FROM sync_copies sc WHERE sc.message=m.id AND sc.origin)",
            );
        } else {
            sql.push_str(" AND EXISTS(SELECT 1 FROM sync_copies sc WHERE sc.message=m.id AND sc.origin AND sc.source=?)");
            bind.push(source.clone().into());
        }
    }
    if let Some(t) = &f.text {
        sql.push_str(" AND instr(board_lower(m.text),?)>0");
        bind.push(t.to_lowercase().into());
    }
    if let Some(after) = f.after {
        sql.push_str(" AND m.id>?");
        bind.push(after.into());
    }
    if let Some(re) = f.re {
        sql.push_str(" AND (m.id=? OR m.re=?)");
        bind.extend([re.into(), re.into()]);
    }
    if let Some(t) = &f.mentions {
        let h = crate::prose::query(t);
        if crate::prose::exact_selector(t).is_some() {
            sql.push_str(
                " AND EXISTS(SELECT 1 FROM mentions u WHERE u.message_id=m.id AND u.handle=?)",
            );
            bind.push(h.into());
        } else {
            sql.push_str(" AND EXISTS(SELECT 1 FROM mentions u WHERE u.message_id=m.id AND (u.handle=? OR instr(u.handle,?)=1))");
            let sep = crate::prose::family_separator(&h);
            bind.push(h.clone().into());
            bind.push(format!("{h}{sep}").into());
        }
    }
    for (k, v) in &f.labels {
        sql.push_str(" AND EXISTS(SELECT 1 FROM labels l WHERE (l.message_id=m.id OR l.message_id=m.re) AND l.key=?");
        bind.push(k.clone().into());
        if !v.is_empty() {
            sql.push_str(" AND l.value IN (SELECT value FROM json_each(?))");
            bind.push(
                serde_json::json!(v.split('|').collect::<Vec<_>>())
                    .to_string()
                    .into(),
            );
        }
        sql.push(')');
    }
    for term in &f.label_terms {
        sql.push_str(" AND EXISTS(SELECT 1 FROM labels l WHERE (l.message_id=m.id OR l.message_id=m.re) AND l.key=?");
        bind.push(term.key.clone().into());
        if let Some(v) = &term.values {
            sql.push_str(" AND l.value IN (SELECT value FROM json_each(?))");
            bind.push(serde_json::json!(v).to_string().into());
        }
        sql.push(')');
    }
    (sql, bind)
}
fn check_channel(c: &Connection, access: &Access, filter: &Filter) -> Result<()> {
    if let Some(channel) = filter.channel.as_ref().filter(|s| !s.ends_with('/')) {
        let found: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM channels WHERE name=?)",
            [channel],
            |r| r.get(0),
        )?;
        ensure!(
            found && access.allows(Action::Read, channel),
            crate::errors::Kind::Missing
                .error("channel_not_found", format!("channel not found: {channel}"))
        );
    }
    Ok(())
}
pub(crate) fn visible_predicate(
    c: &Connection,
    a: &Access,
    f: &Filter,
) -> Result<(String, Vec<Bind>)> {
    f.validate()?;
    let mut base = f.clone();
    let mention = base.mentions.take();
    let author = base.by.take();
    let (mut pred, mut bind) = predicate(&base);
    if let Some(q) = author {
        let authors = crate::actors::author_filter(c, a, &q)?;
        pred.push_str(" AND (m.actor IN (SELECT value FROM json_each(?)) OR
            (instr(m.actor,'/')>0 AND substr(m.actor,1,instr(m.actor,'/')-1) IN (SELECT value FROM json_each(?))))");
        bind.push(serde_json::to_string(&authors.ids)?.into());
        bind.push(serde_json::to_string(&authors.families)?.into());
    }
    if let Some(q) = mention {
        let terms = crate::actors::mention_terms(c, a, &q)?;
        let names = terms
            .iter()
            .map(|t| crate::prose::query(t))
            .collect::<Vec<_>>();
        let prefixes = if crate::prose::exact_selector(&q).is_some() {
            vec![]
        } else {
            names
                .iter()
                .map(|t| format!("{t}{}", crate::prose::family_separator(t)))
                .collect()
        };
        pred.push_str(" AND EXISTS(SELECT 1 FROM mentions u WHERE u.message_id=m.id AND
            (u.handle IN (SELECT value FROM json_each(?)) OR EXISTS(SELECT 1 FROM json_each(?) prefix WHERE instr(u.handle,prefix.value)=1)))");
        bind.push(serde_json::to_string(&names)?.into());
        bind.push(serde_json::to_string(&prefixes)?.into());
    }
    if let Some(channels) = a.channels(c, Action::Read)? {
        pred.push_str(" AND m.channel IN (SELECT value FROM json_each(?))");
        bind.push(serde_json::to_string(&channels)?.into());
    }
    Ok((pred, bind))
}
impl Board {
    pub fn actors(&self) -> Result<Vec<String>> {
        Ok(self
            .actor_directory(None)?
            .into_iter()
            .map(|a| a.id)
            .collect())
    }
    pub fn cursor(&self) -> Result<i64> {
        self.access_on(&self.conn)?;
        cursor_on(&self.conn)
    }
    pub fn thread(&self, id: i64) -> Result<Vec<Message>> {
        self.thread_filtered(id, &Filter::default())
    }
    pub fn thread_filtered(&self, id: i64, f: &Filter) -> Result<Vec<Message>> {
        Ok(self.thread_page(id, f)?.1)
    }
    pub fn thread_page(&self, id: i64, f: &Filter) -> Result<(i64, Vec<Message>)> {
        let conversation = self.conversation(id, f)?;
        let rows = std::iter::once(conversation.message)
            .chain(conversation.replies)
            .collect();
        Ok((conversation.cursor, rows))
    }
    pub fn conversation(&self, id: i64, f: &Filter) -> Result<Conversation> {
        self.conversation_window(id, f, None)
    }
    /// With no cursor, a limit selects the newest replies in chronological order.
    /// A cursor belongs to this thread and selects the next replies instead.
    pub fn conversation_window(
        &self,
        id: i64,
        f: &Filter,
        limit: Option<usize>,
    ) -> Result<Conversation> {
        if let Some(n) = limit {
            ensure!((1..=500).contains(&n), "limit must be 1..500");
        }
        let tx = self.conn.unchecked_transaction()?;
        let a = self.access_on(&tx)?;
        a.message(&tx, id, Action::Read)?;
        let cursor = cursor_on(&tx)?;
        let given = Self::get_on(&tx, id)?;
        let root = if let Some(re) = given.re {
            Self::get_on(&tx, re)?
        } else {
            given
        };
        if let Some(after) = f.after {
            let belongs: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE id=?1 AND (id=?2 OR re=?2))",
                [after, root.id],
                |r| r.get(0),
            )?;
            ensure!(
                belongs,
                crate::errors::Kind::Invalid
                    .error("invalid_after", "after must name a message in this thread")
            );
        }
        let schedules = crate::automation::pending_in_thread(&tx, root.id)?;
        let mut replies = Vec::new();
        let mut filter = f.clone();
        filter.re = Some(root.id);
        let (pred, mut params) = visible_predicate(&tx, &a, &filter)?;
        let latest = limit.is_some() && f.after.is_none();
        let dir = if latest { "DESC" } else { "ASC" };
        let mut sql = format!(
            "SELECT m.id FROM messages m WHERE m.re IS NOT NULL AND {pred} ORDER BY m.id {dir}"
        );
        if let Some(n) = limit {
            sql.push_str(" LIMIT ?");
            params.push((n as i64).into());
        }
        for id in tx
            .prepare(&sql)?
            .query_map(params_from_iter(params), |r| r.get(0))?
        {
            replies.push(a.view(&tx, Self::get_on(&tx, id?)?)?);
        }
        if latest {
            replies.reverse();
        }
        let root = a.view(&tx, root)?;
        tx.commit()?;
        Ok(Conversation {
            message: root,
            replies,
            schedules,
            cursor,
        })
    }
    pub fn messages(&self, f: &Filter, limit: usize) -> Result<Vec<Message>> {
        Ok(self.messages_page(f, limit)?.1)
    }
    pub fn messages_page(&self, f: &Filter, limit: usize) -> Result<(i64, Vec<Message>)> {
        let page = self.messages_window(f, None, limit)?;
        Ok((page.cursor, page.rows))
    }
    pub fn messages_window(
        &self,
        f: &Filter,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Window<Message>> {
        check_window(f.after, before, limit)?;
        let tx = self.conn.unchecked_transaction()?;
        let a = self.access_on(&tx)?;
        check_channel(&tx, &a, f)?;
        let cursor = cursor_on(&tx)?;
        let (mut pred, mut p) = visible_predicate(&tx, &a, f)?;
        if let Some(before) = before {
            pred.push_str(" AND m.id<?");
            p.push(before.into());
        }
        let order = if f.after.is_some() { "ASC" } else { "DESC" };
        p.push(((limit + 1) as i64).into());
        let sql = format!("SELECT m.id FROM messages m WHERE {pred} ORDER BY m.id {order} LIMIT ?");
        let mut ids: Vec<i64> = tx
            .prepare(&sql)?
            .query_map(params_from_iter(p), |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let more = ids.len() > limit;
        ids.truncate(limit);
        let older = (more && f.after.is_none()).then(|| ids[limit - 1]);
        let resume = if more && f.after.is_some() {
            ids[limit - 1]
        } else {
            cursor
        };
        if f.after.is_none() {
            ids.reverse();
        }
        let out = ids
            .into_iter()
            .map(|id| a.view(&tx, Self::get_on(&tx, id)?))
            .collect::<Result<_>>()?;
        tx.commit()?;
        Ok(Window {
            snapshot: cursor,
            cursor: resume,
            older,
            rows: out,
        })
    }
    pub fn threads(&self, f: &Filter, limit: usize) -> Result<Vec<ThreadSummary>> {
        Ok(self.threads_page(f, limit)?.1)
    }
    pub fn threads_page(&self, f: &Filter, limit: usize) -> Result<(i64, Vec<ThreadSummary>)> {
        let page = self.threads_window(f, None, limit)?;
        Ok((page.cursor, page.rows))
    }
    pub fn threads_window(
        &self,
        f: &Filter,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Window<ThreadSummary>> {
        check_window(f.after, before, limit)?;
        let tx = self.conn.unchecked_transaction()?;
        let a = self.access_on(&tx)?;
        check_channel(&tx, &a, f)?;
        let cursor = cursor_on(&tx)?;
        f.validate()?;
        let mut conditions = Vec::new();
        let mut p = Vec::new();
        if let Some(scope) = &f.channel {
            if let Some(base) = scope.strip_suffix('/') {
                conditions.push("(t.channel=? OR instr(t.channel,?)=1)".to_owned());
                p.push(base.to_owned().into());
            } else {
                conditions.push("t.channel=?".to_owned());
            }
            p.push(scope.clone().into());
        }
        if f.by.is_some()
            || f.from.is_some()
            || f.mentions.is_some()
            || f.re.is_some()
            || f.text.is_some()
            || !f.labels.is_empty()
            || !f.label_terms.is_empty()
        {
            let filter = Filter {
                after: None,
                ..f.clone()
            };
            let (pred, params) = visible_predicate(&tx, &a, &filter)?;
            conditions.push(format!(
                "t.root IN (SELECT coalesce(m.re,m.id) FROM messages m WHERE {pred})"
            ));
            p.extend(params);
        } else if f.channel.as_ref().is_none_or(|ch| ch.ends_with('/'))
            && let Some(channels) = a.channels(&tx, Action::Read)?
        {
            conditions.push("t.channel IN (SELECT value FROM json_each(?))".to_owned());
            p.push(serde_json::to_string(&channels)?.into());
        }
        if let Some(n) = before {
            conditions.push("t.activity<?".into());
            p.push(n.into());
        } else if let Some(n) = f.after {
            conditions.push("t.activity>?".into());
            p.push(n.into());
        }
        let pred = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };
        let order = if f.after.is_some() { "ASC" } else { "DESC" };
        p.push(((limit + 1) as i64).into());
        let sql = format!(
            "SELECT t.root,t.activity,t.replies,CASE WHEN t.replies>0 THEN t.activity END
            FROM thread_activity t{pred} ORDER BY t.activity {order} LIMIT ?"
        );
        let mut ids: Vec<(i64, i64, i64, Option<i64>)> = tx
            .prepare(&sql)?
            .query_map(params_from_iter(p), |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let more = ids.len() > limit;
        ids.truncate(limit);
        let older = (more && f.after.is_none()).then(|| ids[limit - 1].1);
        let resume = if more && f.after.is_some() {
            ids[limit - 1].1
        } else {
            cursor
        };
        if f.after.is_none() {
            ids.reverse();
        }
        let mut rows = Vec::new();
        for (id, _, replies, last) in ids {
            rows.push(ThreadSummary {
                message: a.view(&tx, Self::get_on(&tx, id)?)?,
                replies,
                last: last
                    .map(|n| a.view(&tx, Self::get_on(&tx, n)?))
                    .transpose()?,
            });
        }
        tx.commit()?;
        Ok(Window {
            snapshot: cursor,
            cursor: resume,
            older,
            rows,
        })
    }
    pub fn events(&self, after: i64, limit: usize) -> Result<Vec<Event>> {
        Ok(self
            .visible_events(after, limit.min(1000), &Filter::default(), None)?
            .1)
    }
    pub(crate) fn events_on(conn: &Connection, after: i64, limit: usize) -> Result<Vec<Event>> {
        let mut stmt=conn.prepare("SELECT cursor,kind,actor,created_at,payload,message,previous,root_labels FROM events WHERE cursor>? ORDER BY cursor LIMIT ?")?;
        let mut rs = stmt.query(params![after, limit.min(1000) as i64])?;
        let mut out = Vec::new();
        while let Some(r) = rs.next()? {
            let msg = |i: usize| -> Result<Option<Message>> {
                r.get::<_, Option<String>>(i)?
                    .map(|s| serde_json::from_str(&s))
                    .transpose()
                    .context("invalid message in event log")
            };
            out.push(Event {
                cursor: r.get(0)?,
                kind: r.get(1)?,
                actor: r.get(2)?,
                created_at: r.get(3)?,
                payload: serde_json::from_str(&r.get::<_, String>(4)?)?,
                message: msg(5)?,
                previous: msg(6)?,
                root_labels: serde_json::from_str(&r.get::<_, String>(7)?)?,
            });
        }
        Ok(out)
    }
}
pub(crate) fn cursor_on(c: &Connection) -> Result<i64> {
    Ok(
        c.query_row("SELECT coalesce(max(cursor),0) FROM events", [], |r| {
            r.get(0)
        })?,
    )
}
