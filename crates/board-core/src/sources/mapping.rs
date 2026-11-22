use super::*;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
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
fn validate_field(s: &str) -> Result<()> {
    ensure!(
        s.len() <= 64
            && s.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
            && s.bytes().all(|b| b.is_ascii_lowercase()
                || b.is_ascii_digit()
                || matches!(b, b'_' | b'-' | b'.'))
            && !matches!(s, "constructor" | "prototype"),
        "invalid source metadata key: {s}"
    );
    ensure!(
        !s.starts_with("board."),
        Kind::Invalid.error(
            "reserved_label",
            format!("reserved source metadata key: {s}")
        )
    );
    Ok(())
}

fn metadata_on(c: &Connection, id: i64) -> Result<BTreeMap<String, String>> {
    Ok(
        c.prepare("SELECT key,value FROM labels WHERE message_id=?")?
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?,
    )
}

fn put_metadata(tx: &Transaction<'_>, id: i64, fields: &BTreeMap<String, String>) -> Result<usize> {
    let mut changed = 0;
    for (k, v) in fields {
        validate_field(k)?;
        ensure!(
            v.encode_utf16().count() <= 256 && !v.chars().any(char::is_control),
            "source metadata values must be at most 256 characters without controls"
        );
        changed += tx.execute("INSERT INTO labels VALUES (?,?,?) ON CONFLICT(message_id,key) DO UPDATE SET value=excluded.value WHERE value<>excluded.value", params![id,k,v])?;
    }
    Ok(changed)
}

fn update_metadata(
    tx: &Transaction<'_>,
    id: i64,
    source: &str,
    actor: &str,
    set: &BTreeMap<String, String>,
    unset: &[String],
) -> Result<Message> {
    let old = Board::get_on(tx, id)?;
    ensure!(!old.deleted, Kind::Gone.error("deleted", "message removed"));
    let mut changed = put_metadata(tx, id, set)?;
    for k in unset {
        validate_field(k)?;
        ensure!(!set.contains_key(k), "key both set and unset: {k}");
        changed += tx.execute(
            "DELETE FROM labels WHERE message_id=? AND key=?",
            params![id, k],
        )?;
    }
    if changed == 0 {
        return Ok(old);
    }
    Board::changed(
        tx,
        "source.metadata",
        actor,
        &old,
        json!({"source":source}),
        false,
    )
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
            ensure!(
                !tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM private_messages WHERE id IN (?,?))",
                    params![m.id, m.re],
                    |r| r.get::<_, bool>(0)
                )?,
                Kind::Conflict.error("no_sync", "this message is local-only")
            );
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
    ensure!(
        i.links.len() <= 12
            && i.links
                .iter()
                .all(|l| l.message.is_none() && crate::links::slack_address(&l.url).is_some()),
        "invalid external link snapshot"
    );
    ensure!(i.mentioned.len() <= 100, "too many mentioned profiles");
    for (id, p) in &i.mentioned {
        ident(id)?;
        ensure!(
            !id.contains('/')
                && id.len() <= 160
                && !p.name.trim().is_empty()
                && p.name.len() <= 256
                && !p.name.chars().any(char::is_control),
            "invalid mentioned profile"
        );
        valid_link(p.avatar.as_deref())?;
    }
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
fn meet_authors(tx: &Transaction<'_>, s: &Source, i: &Import, actor: &str) -> Result<()> {
    let text = match &i.change {
        Change::Post { text, .. }
        | Change::Upsert { text, .. }
        | Change::Reconcile { text, .. }
        | Change::Edit { text } => text.as_str(),
        Change::Signal {
            reply: Some(text), ..
        } => text,
        _ => "",
    };
    let terms = crate::prose::tokens(text);
    for (id, p) in &i.mentioned {
        let id = format!("source:{}/{}", s.name, id);
        if id != actor && terms.iter().any(|t| t.exact && t.value == id) {
            // Imports can introduce a new person but cannot rename a known actor on their behalf.
            tx.execute(
                "INSERT INTO sync_profiles(actor,source,name,avatar) SELECT ?1,?2,?3,?4
                WHERE NOT EXISTS(SELECT 1 FROM events WHERE actor=?1)
                  AND NOT EXISTS(SELECT 1 FROM actor_profiles WHERE id=?1)
                ON CONFLICT(actor) DO NOTHING",
                params![id, s.name, p.name, p.avatar],
            )?;
        }
    }
    if let Some(p) = &i.profile {
        tx.execute("INSERT INTO sync_profiles VALUES(?,?,?,?) ON CONFLICT(actor) DO UPDATE SET name=excluded.name,avatar=coalesce(excluded.avatar,sync_profiles.avatar)",params![actor,s.name,p.name,p.avatar])?;
    }
    Ok(())
}
pub(super) fn apply(tx: &Transaction<'_>, s: &Source, i: &Import) -> Result<Message> {
    validate(i)?;
    ensure!(
        !matches!(i.change, Change::Ignore),
        Kind::Invalid.error("ignored_event", "provider event has no Board effect")
    );
    let actor = format!("source:{}/{}", s.name, i.author);
    crate::validate_actor(&actor)?;
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
        let root = root_on(tx, &s.name, &i.thread)?
            .with_context(|| Kind::Invalid.error("ignored_event", "signal has no linked thread"))?;
        let m = Board::get_on(tx, root)?;
        let current = metadata_on(tx, root)?;
        if m.deleted || !conditions.iter().all(|(k, v)| current.get(k) == Some(v)) {
            return Ok(m);
        }
        meet_authors(tx, s, i, &actor)?;
        let updated = if set.is_empty() && unset.is_empty() {
            m
        } else {
            update_metadata(tx, root, &s.name, &actor, set, unset)?
        };
        if let Some(text) = reply {
            let m = Board::platform_reply_tx(
                tx,
                root,
                &actor,
                text,
                &PostOptions {
                    no_sync: i.no_sync,
                    ..Default::default()
                },
            )?;
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
            meet_authors(tx, s, i, &actor)?;
            return Board::react_tx(tx, old.id, &actor, emoji, *remove);
        }
        // Copies we delivered can be echoed back by the provider. Do not impersonate the local author.
        if !c.origin {
            return Ok(old);
        }
        let (last,labels):(Option<i64>,String)=tx.query_row("SELECT modified,managed_labels FROM sync_copies WHERE source=? AND thread=? AND external=?",params![s.name,i.thread,i.external],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if stamp.zip(last).is_some_and(|(a, b)| a < b)
            || old.deleted
            || matches!(i.change, Change::Post { .. })
        {
            return Ok(old);
        }
        meet_authors(tx, s, i, &actor)?;
        match &i.change {
            Change::Post { .. } => unreachable!("handled above"),
            Change::Upsert { text, labels: next }
            | Change::Reconcile {
                text, labels: next, ..
            } => {
                let current = metadata_on(tx, old.id)?;
                let prior: BTreeMap<String, String> = serde_json::from_str(&labels)?;
                let mut desired = next.clone();
                let mut extra = Vec::new();
                if let Change::Reconcile { owns, reset, .. } = &i.change {
                    let supplied = |key: &str| {
                        owns.iter().any(|rule| {
                            if rule.ends_with('.') {
                                key.starts_with(rule)
                            } else {
                                key == rule
                            }
                        })
                    };
                    for (k, v) in &prior {
                        if !desired.contains_key(k) && !supplied(k) {
                            desired.insert(k.clone(), v.clone());
                        }
                    }
                    extra.extend(
                        current
                            .keys()
                            .filter(|k| supplied(k) && !next.contains_key(*k))
                            .cloned(),
                    );
                    for (k, prefix) in reset {
                        if next.contains_key(k) && next.get(k) != current.get(k) {
                            ensure!(!prefix.is_empty(), "empty label reset prefix");
                            extra.extend(
                                current
                                    .keys()
                                    .filter(|key| key.starts_with(prefix))
                                    .cloned(),
                            );
                        }
                    }
                }
                let mut unset = prior
                    .iter()
                    .filter(|(k, v)| !desired.contains_key(*k) && current.get(*k) == Some(*v))
                    .map(|(k, _)| k.clone())
                    .collect::<Vec<_>>();
                unset.append(&mut extra);
                unset.sort();
                unset.dedup();
                if text != &old.text || !old.blocks.is_empty() {
                    Board::platform_edit_tx(tx, old.id, &actor, text, None)?;
                }
                if !unset.is_empty() || next.iter().any(|(k, v)| current.get(k) != Some(v)) {
                    update_metadata(tx, old.id, &s.name, &actor, next, &unset)?;
                }
                managed = Some(serde_json::to_string(&desired)?);
                Board::get_on(tx, old.id)?
            }
            Change::Edit { text } => {
                if old.text == *text && old.blocks.is_empty() {
                    old
                } else {
                    Board::platform_edit_tx(tx, old.id, &actor, text, None)?
                }
            }
            Change::Delete => Board::platform_delete_tx(tx, old.id, &actor, None)?,
            Change::React { .. } => unreachable!("handled independently of message version"),
            Change::Signal { .. } | Change::Ignore => {
                unreachable!("handled before looking up the copy")
            }
            Change::Labels { set, unset } => {
                update_metadata(tx, old.id, &s.name, &actor, set, unset)?
            }
        }
    } else {
        ensure!(
            !matches!(
                i.change,
                Change::Reconcile {
                    existing_only: true,
                    ..
                }
            ),
            Kind::Invalid.error("ignored_event", "update has no linked thread")
        );
        let (text, labels) = match &i.change {
            Change::Post { text, labels }
            | Change::Upsert { text, labels }
            | Change::Reconcile { text, labels, .. } => (text, labels),
            _ => {
                return Err(Kind::Conflict
                    .error("source_parent", "original message has not arrived")
                    .into());
            }
        };
        let opts = PostOptions {
            no_sync: i.no_sync,
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
            None => match root_on(tx, &s.name, &i.thread)? {
                None => None,
                Some(local) => {
                    // A join links conversations without equating their two original roots.
                    // Import that root once, as a distinct message, so later reply targets stay exact.
                    let first = tx.execute("UPDATE sync_explicit_joins SET remote_root=? WHERE source=? AND thread=? AND remote_root IS NULL",params![i.external,s.name,i.thread])?;
                    ensure!(
                        first == 1,
                        Kind::Conflict.error(
                            "already_synced",
                            "this external thread already has a different root"
                        )
                    );
                    Some(local)
                }
            },
        };
        meet_authors(tx, s, i, &actor)?;
        let m = match parent {
            Some(p) => Board::platform_reply_tx(tx, p, &actor, text, &opts)?,
            None => Board::platform_post_tx(tx, &s.channel, &actor, text, &opts)?,
        };
        put_metadata(tx, m.id, labels)?;
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
