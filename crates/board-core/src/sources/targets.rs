use super::*;
use std::collections::BTreeSet;

pub fn validate_slack_timestamp(value: &str) -> Result<()> {
    let (seconds, fraction) = value
        .split_once('.')
        .context("expected a Slack message timestamp")?;
    ensure!(
        (1..=16).contains(&seconds.len())
            && (1..=9).contains(&fraction.len())
            && seconds
                .bytes()
                .chain(fraction.bytes())
                .all(|b| b.is_ascii_digit()),
        "invalid Slack timestamp"
    );
    Ok(())
}

fn join_thread(provider: &Provider, value: &str) -> Result<String> {
    ident(value)?;
    ensure!(
        !value.starts_with("pending:"),
        "expected a provider thread, not a pending lane"
    );
    match provider {
        Provider::Github => {
            let raw = value.strip_prefix('#').unwrap_or(value);
            ensure!(
                raw.bytes().all(|b| b.is_ascii_digit()),
                "expected a positive issue or pull number"
            );
            Ok(raw
                .parse::<u64>()
                .ok()
                .filter(|n| *n > 0)
                .context("expected a positive issue or pull number")?
                .to_string())
        }
        Provider::Slack => {
            validate_slack_timestamp(value)?;
            Ok(value.into())
        }
        Provider::Linear => Ok(uuid::Uuid::parse_str(value)
            .context("an explicit Linear join needs the issue UUID")?
            .to_string()),
        Provider::Local => Ok(value.into()),
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ThreadLink {
    pub source: String,
    pub thread: String,
    pub root: i64,
}

// Recover the publication boundary on early v12 databases. Do not erase a confirmed
// receipt or claim that a send already in flight never reached the provider.
pub(super) fn migrate_joins(c: &Connection) -> Result<()> {
    if !c.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('sync_explicit_joins') WHERE name='joined_at')", [], |r| r.get::<_, bool>(0))? {
        c.execute_batch("ALTER TABLE sync_explicit_joins ADD COLUMN joined_at INTEGER NOT NULL DEFAULT 0;
            UPDATE sync_explicit_joins AS j SET joined_at=coalesce(
                (SELECT min(e.cursor) FROM events e JOIN sync_threads t ON t.root=e.message_id
                 WHERE t.source=j.source AND t.thread=j.thread AND e.kind='thread.sync_requested'
                 AND json_extract(e.payload,'$.source')=j.source AND json_extract(e.payload,'$.thread')=j.thread),
                (SELECT coalesce(max(cursor),0) FROM events));
            UPDATE sync_queue AS q SET state='skipped',receipt=NULL,deadline=NULL,error='message predates thread join'
                WHERE direction='out' AND state IN ('pending','leased') AND EXISTS(
                SELECT 1 FROM sync_explicit_joins j WHERE j.source=q.source AND j.thread=q.thread
                AND json_extract(q.payload,'$.message')<=j.joined_at);")?;
    }
    Ok(())
}

impl Board {
    pub fn source_thread_link(&self, who: &str, name: &str, thread: &str) -> Result<ThreadLink> {
        let tx = self.conn.unchecked_transaction()?;
        let s = self.authorized_source(&tx, name, who)?;
        let thread = join_thread(&s.provider, thread)?;
        let root = mapping::root_on(&tx, name, &thread)?
            .with_context(|| Kind::Missing.error("source_not_found", "no linked conversation"))?;
        self.access_on(&tx)?
            .message(&tx, root, crate::access::Action::Read)?;
        tx.commit()?;
        Ok(ThreadLink {
            source: s.name,
            thread,
            root,
        })
    }

    /// Attach conversations, not messages. Neither local history nor the remote original is a writable copy.
    pub fn join_source_thread(
        &self,
        who: &str,
        name: &str,
        thread: &str,
        root: i64,
    ) -> Result<(ThreadLink, bool)> {
        self.write(|tx| {
            let s = self.authorized_source(tx, name, who)?;
            self.access_on(tx)?
                .message(tx, root, crate::access::Action::Read)?;
            let m = Self::get_on(tx, root)?;
            let thread = join_thread(&s.provider, thread)?;
            let fresh = Self::join_source_thread_on(tx, who, &s, &thread, &m)?;
            Ok((
                ThreadLink {
                    source: s.name,
                    thread,
                    root,
                },
                fresh,
            ))
        })
    }

    fn join_source_thread_on(
        tx: &Transaction<'_>,
        who: &str,
        s: &Source,
        thread: &str,
        root: &Message,
    ) -> Result<bool> {
        ensure!(s.outbound, "publishing must be explicitly enabled");
        ensure!(
            s.channel == root.channel && root.re.is_none() && !root.no_sync && !root.deleted,
            "expected a live shareable root in the mapped channel"
        );
        if let Some(existing) = mapping::root_on(tx, &s.name, thread)? {
            ensure!(
                existing == root.id,
                Kind::Conflict.error(
                    "already_linked",
                    "that conversation is already linked to another local thread"
                )
            );
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO sync_threads(source,thread,root) VALUES(?,?,?)",
            params![s.name, thread, root.id],
        )?;
        let cursor = Self::event_tx(
            tx,
            "thread.sync_requested",
            who,
            Some(root.id),
            json!({"source":s.name,"thread":thread,"kind":"join"}),
        )?;
        tx.execute(
            "INSERT INTO sync_explicit_joins(source,thread,joined_at) VALUES(?,?,?)",
            params![s.name, thread, cursor],
        )?;
        Ok(true)
    }

    pub(crate) fn queue_post_targets_on(
        &self,
        tx: &Transaction<'_>,
        who: &str,
        root: &Message,
        targets: &[String],
    ) -> Result<()> {
        let mut starts = BTreeSet::new();
        let mut joins = BTreeSet::new();
        for target in targets {
            match target.split_once(':') {
                Some((name, thread)) => {
                    let s = self.authorized_source(tx, name, who)?;
                    joins.insert((name, join_thread(&s.provider, thread)?));
                }
                None => {
                    starts.insert(target.as_str());
                }
            }
        }
        for name in &starts {
            self.start_source_thread_on(tx, who, name, root.id)?;
        }
        for (name, normalized) in joins {
            let thread = normalized.as_str();
            let s = self.authorized_source(tx, name, who)?;
            if !starts.contains(name) {
                // Joining overrides this source's automatic start, but never an explicitly requested start.
                // Only this transaction's as-yet-invisible automatic job has this reserved lane.
                let lane = format!("pending:board:{}", root.id);
                let automatic = tx.execute("DELETE FROM sync_starts WHERE source=?1 AND root=?2 AND item IN (SELECT id FROM sync_queue WHERE source=?1 AND direction='out' AND thread=?3 AND state='pending' AND attempts=0)",params![name,root.id,lane])?;
                if automatic != 0 {
                    tx.execute(
                        "DELETE FROM sync_queue WHERE source=? AND direction='out' AND thread=?",
                        params![name, lane],
                    )?;
                    tx.execute(
                        "DELETE FROM sync_threads WHERE source=? AND root=? AND thread=?",
                        params![name, root.id, lane],
                    )?;
                }
            }
            Self::join_source_thread_on(tx, who, &s, thread, root)?;
        }
        Ok(())
    }
}
