use super::*;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Copy {
    pub source: String,
    pub thread: String,
    pub external: String,
    pub message: i64,
    pub origin: bool,
    pub url: Option<String>,
}

fn copy_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Copy> {
    Ok(Copy {
        source: r.get(0)?,
        thread: r.get(1)?,
        external: r.get(2)?,
        message: r.get(3)?,
        origin: r.get(4)?,
        url: r.get(5)?,
    })
}
pub(crate) fn origin_on(c: &Connection, id: i64) -> Result<Option<Copy>> {
    Ok(c.query_row("SELECT source,thread,external,message,origin,url FROM sync_copies WHERE message=? AND origin=1 ORDER BY source,thread LIMIT 1",[id],copy_row).optional()?)
}
pub(super) fn root_on(c: &Connection, name: &str, thread: &str) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT root FROM sync_threads WHERE source=? AND thread=?",
        params![name, thread],
        |r| r.get(0),
    )
    .optional()?)
}
pub(super) fn find_on(c: &Connection, name: &str, thread: &str, id: &str) -> Result<Option<Copy>> {
    Ok(c.query_row("SELECT source,thread,external,message,origin,url FROM sync_copies WHERE source=? AND thread=? AND external=?",params![name,thread,id],copy_row).optional()?)
}
fn valid_link(url: Option<&str>) -> Result<()> {
    if let Some(u) = url {
        ensure!(
            u.len() <= 4096
                && (u.starts_with("https://") || u.starts_with("http://"))
                && !u.chars().any(|c| c.is_control() || c.is_whitespace()),
            "expected an HTTP(S) permalink"
        );
    }
    Ok(())
}
pub(super) fn bind(
    tx: &Transaction<'_>,
    s: &Source,
    t: &str,
    external: &str,
    m: &Message,
    origin: bool,
    url: Option<&str>,
) -> Result<Copy> {
    ident(t)?;
    ident(external)?;
    valid_link(url)?;
    ensure!(
        m.channel == s.channel,
        Kind::Conflict.error(
            "source_channel",
            "message is outside the configured source channel"
        )
    );
    let root = m.re.unwrap_or(m.id);
    match root_on(tx, &s.name, t)? {
        None => {
            ensure!(
                m.re.is_none(),
                Kind::Conflict.error("source_parent", "attach a root before its replies")
            );
            tx.execute(
                "INSERT INTO sync_threads VALUES(?,?,?)",
                params![s.name, t, root],
            )?;
        }
        Some(other) => ensure!(
            other == root,
            Kind::Conflict.error(
                "source_thread",
                "external thread is already bound elsewhere"
            )
        ),
    }
    tx.execute("INSERT OR IGNORE INTO sync_copies(source,thread,external,message,origin,url) VALUES(?,?,?,?,?,?)",params![s.name,t,external,m.id,origin,url])?;
    let copy = find_on(tx, &s.name, t, external)?.with_context(|| {
        Kind::Conflict.error("source_copy", "message has another copy at this target")
    })?;
    ensure!(
        copy.message == m.id,
        Kind::Conflict.error(
            "source_copy",
            "external id is already bound to another message"
        )
    );
    Ok(copy)
}
impl Board {
    pub fn attach_source(
        &self,
        who: &str,
        name: &str,
        thread: &str,
        external: &str,
        local: i64,
        url: Option<&str>,
    ) -> Result<Copy> {
        self.write(|tx| {
            let s = self.authorized_source(tx, name, who)?;
            let m = Self::get_on(tx, local)?;
            let old = find_on(tx, name, thread, external)?;
            let item = bind(tx, &s, thread, external, &m, false, url)?;
            if old.is_none() {
                Self::event_tx(
                    tx,
                    "thread.linked",
                    who,
                    Some(local),
                    json!({"source":name,"thread":thread,"external":external}),
                )?;
            }
            Ok(item)
        })
    }
    pub fn find_source(
        &self,
        who: &str,
        name: &str,
        thread: &str,
        external: Option<&str>,
    ) -> Result<Option<Message>> {
        let tx = self.conn.unchecked_transaction()?;
        self.authorized_source(&tx, name, who)?;
        let id = match external {
            Some(key) => find_on(&tx, name, thread, key)?.map(|x| x.message),
            None => root_on(&tx, name, thread)?,
        };
        let access = self.access_on(&tx)?;
        let out = id
            .map(|id| access.view(&tx, Self::get_on(&tx, id)?))
            .transpose()?;
        tx.commit()?;
        Ok(out)
    }
    pub fn source_copies(&self, who: &str, name: &str, local: i64) -> Result<Vec<Copy>> {
        let tx = self.conn.unchecked_transaction()?;
        self.authorized_source(&tx, name, who)?;
        let rows=tx.prepare("SELECT source,thread,external,message,origin,url FROM sync_copies WHERE source=? AND message=? ORDER BY thread")?.query_map(params![name,local],copy_row)?.collect::<rusqlite::Result<_>>()?;
        tx.commit()?;
        Ok(rows)
    }
    pub fn linked_source(&self, who: &str, name: &str, url: &str) -> Result<Option<Message>> {
        let tx = self.conn.unchecked_transaction()?;
        self.authorized_source(&tx, name, who)?;
        let id = tx
            .query_row(
                "SELECT message FROM sync_copies WHERE source=? AND url=? ORDER BY message LIMIT 1",
                params![name, url],
                |r| r.get(0),
            )
            .optional()?;
        let access = self.access_on(&tx)?;
        let m = id
            .map(|i| access.view(&tx, Self::get_on(&tx, i)?))
            .transpose()?;
        tx.commit()?;
        Ok(m)
    }
    /// Resolve a provider-wide message id only when it identifies exactly one conversation.
    pub fn source_thread_for(
        &self,
        who: &str,
        name: &str,
        external: &str,
    ) -> Result<Option<String>> {
        let tx = self.conn.unchecked_transaction()?;
        self.authorized_source(&tx, name, who)?;
        let candidates=tx.prepare("SELECT thread FROM sync_copies WHERE source=? AND external=? ORDER BY thread LIMIT 2")?
            .query_map(params![name,external],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        ensure!(
            candidates.len() < 2,
            Kind::Conflict.error("ambiguous_copy", "external id matches several threads")
        );
        tx.commit()?;
        Ok(candidates.into_iter().next())
    }
}

pub(super) fn validate(i: &Import) -> Result<()> {
    ident(&i.thread)?;
    ident(&i.external)?;
    ident(&i.author)?;
    if let Some(p) = &i.parent {
        ident(p)?;
        ensure!(
            p != &i.external,
            "an external message cannot reply to itself"
        );
    }
    ensure!(
        i.author.len() <= 160 && !i.author.contains('/'),
        "invalid external author"
    );
    valid_link(i.url.as_deref())?;
    if let Some(p) = &i.profile {
        ensure!(
            !p.name.trim().is_empty()
                && p.name.len() <= 256
                && !p.name.chars().any(char::is_control),
            "invalid external display name"
        );
        valid_link(p.avatar.as_deref())?;
    }
    ensure!(
        serde_json::to_vec(i)?.len() <= 128 * 1024,
        "normalized import is too large"
    );
    Ok(())
}
pub(super) fn apply(tx: &Transaction<'_>, s: &Source, i: &Import) -> Result<Message> {
    validate(i)?;
    let actor = format!("source:{}/{}", s.name, i.author);
    crate::validate_actor(&actor)?;
    if let Some(p) = &i.profile {
        tx.execute("INSERT INTO sync_profiles VALUES(?,?,?,?) ON CONFLICT(actor) DO UPDATE SET name=excluded.name,avatar=excluded.avatar",params![actor,s.name,p.name,p.avatar])?;
    }
    let copy = find_on(tx, &s.name, &i.thread, &i.external)?;
    if let Change::Signal {
        conditions,
        set,
        unset,
        reply,
    } = &i.change
    {
        if let Some(c) = copy {
            return Board::get_on(tx, c.message);
        }
        let root = root_on(tx, &s.name, &i.thread)?.context("signal needs a linked root")?;
        let m = Board::get_on(tx, root)?;
        if m.deleted || !conditions.iter().all(|(k, v)| m.labels.get(k) == Some(v)) {
            return Ok(m);
        }
        let updated = Board::labels_tx(tx, root, &actor, set, unset, &BTreeMap::new())?;
        if let Some(text) = reply {
            let m = Board::reply_tx(tx, root, &actor, text, &Default::default())?;
            bind(tx, s, &i.thread, &i.external, &m, true, i.url.as_deref())?;
            let m = Board::get_on(tx, m.id)?;
            Board::snapshot(tx, m.id, &m, None)?;
            return Ok(m);
        }
        return Ok(updated);
    }
    let stamp = i.updated_at.map(|x| x.timestamp_millis());
    let mut managed = None;
    let m = if let Some(c) = copy {
        let old = Board::get_on(tx, c.message)?;
        // A reaction belongs to its author, not the author/version of the message.
        if let Change::React { emoji, remove } = &i.change {
            if old.deleted {
                return Ok(old);
            }
            return Board::react_tx(tx, old.id, &actor, emoji, *remove);
        }
        // Copies we delivered can be echoed back by the provider. Do not impersonate the local author.
        if !c.origin {
            return Ok(old);
        }
        let (last,labels):(Option<i64>,String)=tx.query_row("SELECT modified,managed_labels FROM sync_copies WHERE source=? AND thread=? AND external=?",params![s.name,i.thread,i.external],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if stamp.zip(last).is_some_and(|(a, b)| a < b) || old.deleted {
            return Ok(old);
        }
        match &i.change {
            Change::Post { .. } => return Ok(old),
            Change::Upsert { text, labels: next } => {
                let prior: BTreeMap<String, String> = serde_json::from_str(&labels)?;
                let unset = prior
                    .iter()
                    .filter(|(k, v)| !next.contains_key(*k) && old.labels.get(*k) == Some(*v))
                    .map(|(k, _)| k.clone())
                    .collect::<Vec<_>>();
                if text != &old.text {
                    Board::edit_tx(tx, old.id, &old.actor, text, None)?;
                }
                if !unset.is_empty() || next.iter().any(|(k, v)| old.labels.get(k) != Some(v)) {
                    Board::labels_tx(tx, old.id, &actor, next, &unset, &BTreeMap::new())?;
                }
                managed = Some(serde_json::to_string(next)?);
                Board::get_on(tx, old.id)?
            }
            Change::Edit { text } => {
                if old.text == *text {
                    old
                } else {
                    Board::edit_tx(tx, old.id, &old.actor, text, None)?
                }
            }
            Change::Delete => Board::delete_tx(tx, old.id, &old.actor, None)?,
            Change::React { .. } => unreachable!("handled independently of message version"),
            Change::Signal { .. } => unreachable!("handled on the linked root"),
            Change::Labels { set, unset } => {
                Board::labels_tx(tx, old.id, &actor, set, unset, &BTreeMap::new())?
            }
        }
    } else {
        let (text, labels) = match &i.change {
            Change::Post { text, labels } | Change::Upsert { text, labels } => (text, labels),
            _ => {
                return Err(Kind::Conflict
                    .error("source_parent", "original message has not arrived")
                    .into());
            }
        };
        let opts = PostOptions {
            labels: labels.clone(),
            ..Default::default()
        };
        let parent = match &i.parent {
            Some(p) => Some(
                find_on(tx, &s.name, &i.thread, p)?
                    .with_context(|| {
                        Kind::Conflict.error("source_parent", "external parent has not arrived")
                    })?
                    .message,
            ),
            None => root_on(tx, &s.name, &i.thread)?,
        };
        let m = match parent {
            Some(p) => Board::reply_tx(tx, p, &actor, text, &opts)?,
            None => Board::post_tx(tx, &s.channel, &actor, text, &opts)?,
        };
        bind(tx, s, &i.thread, &i.external, &m, true, i.url.as_deref())?;
        let m = Board::get_on(tx, m.id)?;
        Board::snapshot(tx, m.id, &m, None)?;
        managed = Some(serde_json::to_string(labels)?);
        Board::event_tx(
            tx,
            "message.linked",
            &actor,
            Some(m.id),
            json!({"source":s.name,"thread":i.thread,"external":i.external}),
        )?;
        m
    };
    tx.execute("UPDATE sync_copies SET modified=coalesce(?,modified),url=coalesce(?,url),managed_labels=coalesce(?,managed_labels) WHERE source=? AND thread=? AND external=?",params![stamp,i.url,managed,s.name,i.thread,i.external])?;
    Ok(m)
}
