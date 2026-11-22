use super::*;
use crate::core::PublicDestination;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SwarmFilter {
    pub kind: Option<String>,
    pub worker: Option<String>,
}

pub(super) fn dm_on(c: &Connection, id: i64) -> Result<SwarmDm> {
    Ok(c.query_row("SELECT id,run_id,from_actor,to_worker,text,event_cursor,created_at FROM swarm_dms WHERE id=?",[id],|r|Ok(SwarmDm{id:r.get(0)?,run_id:r.get(1)?,from_actor:r.get(2)?,to_worker:r.get(3)?,text:r.get(4)?,event_cursor:r.get(5)?,created_at:r.get(6)?}))?)
}
pub(crate) fn read_mention_routing(c: &Connection, m: &mut Message) -> Result<()> {
    let mut stmt = c.prepare_cached(
        "SELECT target_worker AS target, 1 FROM swarm_notifications
            WHERE message_id=?1 AND source_kind='mention'
         UNION ALL SELECT json_extract(payload,'$.target'), 0 FROM events
            WHERE message_id=?1 AND kind='swarm.notification_suppressed'
         ORDER BY target",
    )?;
    let mut rows = stmt.query([m.id])?;
    m.notified.clear();
    m.suppressed.clear();
    while let Some(row) = rows.next()? {
        let targets = if row.get::<_, bool>(1)? {
            &mut m.notified
        } else {
            &mut m.suppressed
        };
        targets.push(row.get(0)?);
    }
    Ok(())
}
pub(super) fn notify(
    tx: &Transaction,
    run: &SwarmRun,
    actor: &str,
    target: &str,
    kind: &str,
    at: i64,
    source: Option<i64>,
) -> Result<()> {
    let msg = source.filter(|_| kind == "mention");
    let dm = source.filter(|_| kind == "dm");
    tx.execute("INSERT INTO swarm_notifications(run_id,target_worker,source_kind,from_actor,message_id,dm_id,event_cursor,created_at) VALUES(?,?,?,?,?,?,?,?)",params![run.id,target,kind,actor,msg,dm,at,crate::now()])?;
    Ok(())
}
pub(super) fn spend(tx: &Transaction, run: &SwarmRun, actor: &str, n: i64) -> Result<()> {
    if n == 0 {
        return Ok(());
    }
    tx.execute(
        "UPDATE swarm_runs SET notifications_sent=notifications_sent+? WHERE id=?",
        params![n, run.id],
    )?;
    if run.notifications_sent < run.max_notifications
        && run.notifications_sent + n >= run.max_notifications
    {
        let c = Board::event_tx(
            tx,
            "swarm.budget_reached",
            actor,
            Some(run.root_id),
            serde_json::json!({"run":run.slug,"limit":run.max_notifications}),
        )?;
        notify(tx, run, actor, "coordinator", "system", c, None)?;
    }
    Ok(())
}
fn live(w: &SwarmWorker) -> bool {
    !matches!(w.status.as_str(), "done" | "stopped")
}
fn route_public(
    tx: &Transaction,
    run: &SwarmRun,
    m: &Message,
    targets: BTreeSet<String>,
) -> Result<()> {
    tx.execute(
        "UPDATE swarm_workers SET last_activity_at=? WHERE run_id=? AND actor=?",
        params![crate::now(), run.id, m.actor],
    )?;
    let mut used = 0;
    for who in targets {
        let peer = who != "coordinator" && !run.can_control;
        if peer && run.notifications_sent + used >= run.max_notifications {
            Board::event_tx(
                tx,
                "swarm.notification_suppressed",
                &m.actor,
                Some(m.id),
                serde_json::json!({"run":run.slug,"target":who}),
            )?;
        } else {
            notify(tx, run, &m.actor, &who, "mention", m.id, Some(m.id))?;
            used += i64::from(peer);
        }
    }
    spend(tx, run, &m.actor, used)
}
fn resolve(run: &SwarmRun, actor: &str, text: &str) -> (String, BTreeSet<String>) {
    let mut out = String::new();
    let mut at = 0;
    let mut hits = BTreeSet::new();
    let mut tokens = crate::prose::tokens(text);
    // Unlike an ordinary handle, a swarm slug may end in a hyphen or underscore.
    if run.workers.iter().any(|w| w.name.ends_with(['-', '_'])) {
        let prose = crate::prose::mention_prose(text).to_ascii_lowercase();
        for w in run.workers.iter().filter(|w| w.name.ends_with(['-', '_'])) {
            for (start, s) in prose.match_indices(&format!("@{}", w.name)) {
                let end = start + s.len();
                if tokens
                    .iter()
                    .any(|t| t.range.start < end && start < t.range.end)
                {
                    continue;
                }
                if prose[..start].chars().next_back().is_some_and(|c| {
                    c.is_ascii_alphanumeric() || matches!(c, '@' | '_' | '-' | '.' | '/')
                }) || prose[end..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '/'))
                    || prose[end..]
                        .strip_prefix('.')
                        .is_some_and(|s| s.starts_with(|c: char| c.is_ascii_alphanumeric()))
                {
                    continue;
                }
                tokens.push(crate::prose::Mention {
                    value: &text[start + 1..end],
                    exact: false,
                    range: start..end,
                });
            }
        }
        tokens.sort_by_key(|t| t.range.start);
        tokens.dedup_by_key(|t| t.range.start);
    }
    for token in tokens {
        let val = token.value;
        let member = run
            .workers
            .iter()
            .find(|w| w.actor == val || (!token.exact && w.name.eq_ignore_ascii_case(val)));
        let coordinator = (!token.exact && val.eq_ignore_ascii_case("coordinator"))
            || (val == run.coordinator_actor && member.is_none());
        let replacement = if coordinator {
            if run.coordinator_actor != actor {
                hits.insert("coordinator".to_owned());
            }
            Some(&run.coordinator_actor)
        } else if let Some(w) = member {
            if w.actor != actor && live(w) {
                hits.insert(w.name.clone());
            }
            Some(&w.actor)
        } else {
            None
        };
        if let Some(id) =
            replacement.filter(|s| !s.contains(']') && !s.chars().any(char::is_whitespace))
        {
            out.push_str(&text[at..token.range.start]);
            out.push_str(&format!("@[{id}]"));
            at = token.range.end;
        }
    }
    out.push_str(&text[at..]);
    (out, hits)
}
impl Board {
    pub fn swarm_post(
        &self,
        actor: &str,
        run: &str,
        text: &str,
        opts: &PostOptions,
    ) -> Result<SwarmPostResult> {
        self.write(|tx| {
            let (a, r) = self.swarm_on(tx, actor, run, Action::Post)?;
            r.active()?;
            self.swarm_post_on(tx, &a, &r, actor, text, opts)
        })
    }
    pub(super) fn swarm_post_on(
        &self,
        tx: &Transaction,
        a: &Access,
        r: &SwarmRun,
        actor: &str,
        text: &str,
        opts: &PostOptions,
    ) -> Result<SwarmPostResult> {
        Ok(self
            .publish_public_on(tx, a, actor, PublicDestination::Run(r), text, opts)?
            .0)
    }

    pub(crate) fn publish_public_on(
        &self,
        tx: &Transaction,
        a: &Access,
        actor: &str,
        destination: PublicDestination<'_>,
        text: &str,
        opts: &PostOptions,
    ) -> Result<(SwarmPostResult, bool)> {
        a.actor(actor)?;
        let reply_channel;
        let (channel, parent, known_run) = match destination {
            PublicDestination::Channel(channel) => (channel, None, None),
            PublicDestination::Reply(id) => {
                reply_channel = a.message(tx, id, Action::Post)?;
                (reply_channel.as_str(), Some(id), None)
            }
            PublicDestination::Run(r) => (r.channel.as_str(), None, Some(r)),
        };
        a.require(Action::Post, channel)?;
        a.files(tx, actor, &opts.attachment_ids)?;
        if !opts.labels.is_empty() {
            a.require(Action::Label, channel)?;
        }
        // A blind post must not resolve names against a roster the writer cannot read.
        let blind = !a.allows(Action::Read, channel);
        let loaded = if known_run.is_none() && !blind && channel.starts_with("swarms/") {
            let slug: Option<String> = tx
                .query_row(
                    "SELECT slug FROM swarm_runs WHERE channel=?",
                    [channel],
                    |r| r.get(0),
                )
                .optional()?;
            slug.map(|slug| {
                self.swarm_on(tx, actor, &slug, Action::Post)
                    .map(|(_, r)| r)
            })
            .transpose()?
        } else {
            None
        };
        let run = known_run.or(loaded.as_ref());
        let active = run.filter(|r| r.status == "active");
        let (within, mut hits) = match active {
            Some(r) => {
                let (body, hits) = resolve(r, actor, text);
                (std::borrow::Cow::Owned(body), hits)
            }
            None => (std::borrow::Cow::Borrowed(text), BTreeSet::new()),
        };
        let (body, resolutions) = crate::actors::settle(tx, a, actor, parent, &within)?;
        let (m, created) = match parent {
            Some(id) => Board::reply_tx_with_outcome(tx, id, actor, &body, opts)?,
            None => Board::post_tx_with_outcome(tx, channel, actor, &body, opts)?,
        };
        if created {
            let target = super::coordination::record_on(tx, a, run, &m)?;
            if let Some(r) = run {
                if let Some(target) = target {
                    hits.insert(target);
                }
                if active.is_some() {
                    for person in resolutions.iter().filter_map(|v| v.actor.as_ref()) {
                        if person.id != actor
                            && let Some(name) = r.name_for(&person.id)
                            && (name == "coordinator"
                                || r.workers.iter().any(|w| w.name == name && live(w)))
                        {
                            hits.insert(name.to_owned());
                        }
                    }
                }
                route_public(tx, r, &m, hits)?;
            }
        }
        let message = if blind {
            if created {
                tx.execute("INSERT INTO blind_posts VALUES(?)", [m.id])?;
            }
            ensure!(
                a.blind_on(tx, m.id)?,
                Conflict.error("duplicate_id", "client key belongs to a different write")
            );
            a.original(tx, m.id)?
        } else {
            a.view(tx, m)?
        };
        Ok((
            SwarmPostResult {
                notified: message.notified.clone(),
                suppressed: message.suppressed.clone(),
                message: crate::actors::write_view(message, &body, resolutions, blind),
            },
            created,
        ))
    }
    pub fn swarm_dm(&self, actor: &str, run: &str, to_worker: &str, text: &str) -> Result<SwarmDm> {
        ensure!(
            !text.trim().is_empty()
                && !text.contains('\0')
                && text.encode_utf16().count() <= 10_000,
            Invalid.error("invalid_dm", "DM requires 1..10000 characters")
        );
        self.write(|tx|{let (_,r)=self.swarm_on(tx,actor,run,Action::Post)?;r.active()?;
            if to_worker!="coordinator"{
                let w=r.workers.iter().find(|w|w.name==to_worker).context(Missing.error("worker_not_found","unknown target worker"))?;
                ensure!(live(w),Conflict.error("worker_stopped","target worker has finished"));
            }
            let peer=to_worker!="coordinator" && !r.can_control;
            ensure!(!peer||r.notifications_sent<r.max_notifications,Conflict.error("swarm_budget_exhausted","peer wake limit reached; contact the coordinator"));
            // The global log supplies sequence numbers, but never contains private DM content.
            let at=Board::event_tx(tx,"swarm.dm",actor,None,serde_json::json!({}))?;
            tx.execute("INSERT INTO swarm_dms(run_id,from_actor,to_worker,text,event_cursor,created_at) VALUES(?,?,?,?,?,?)",params![r.id,actor,to_worker,text,at,crate::now()])?;
            let id=tx.last_insert_rowid();notify(tx,&r,actor,to_worker,"dm",at,Some(id))?;
            tx.execute("UPDATE swarm_workers SET last_activity_at=? WHERE run_id=? AND actor=?",params![crate::now(),r.id,actor])?;
            spend(tx,&r,actor,i64::from(peer))?;dm_on(tx,id)})
    }
    pub fn swarm_read(
        &self,
        actor: &str,
        run: &str,
        after: i64,
        limit: usize,
        include_dms_for_all: bool,
    ) -> Result<SwarmFeed> {
        self.swarm_read_filtered(
            actor,
            run,
            after,
            limit,
            include_dms_for_all,
            &SwarmFilter::default(),
        )
    }
    pub fn swarm_read_filtered(
        &self,
        actor: &str,
        run: &str,
        after: i64,
        limit: usize,
        include_dms_for_all: bool,
        filter: &SwarmFilter,
    ) -> Result<SwarmFeed> {
        self.swarm_entries(actor, run, after, limit, include_dms_for_all, filter, false)
    }
    pub fn swarm_latest(
        &self,
        actor: &str,
        run: &str,
        before: Option<i64>,
        limit: usize,
        include_dms_for_all: bool,
    ) -> Result<SwarmFeed> {
        self.swarm_latest_filtered(
            actor,
            run,
            before,
            limit,
            include_dms_for_all,
            &SwarmFilter::default(),
        )
    }
    pub fn swarm_latest_filtered(
        &self,
        actor: &str,
        run: &str,
        before: Option<i64>,
        limit: usize,
        include_dms_for_all: bool,
        filter: &SwarmFilter,
    ) -> Result<SwarmFeed> {
        self.swarm_entries(
            actor,
            run,
            before.unwrap_or(i64::MAX),
            limit,
            include_dms_for_all,
            filter,
            true,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn swarm_entries(
        &self,
        actor: &str,
        run: &str,
        bound: i64,
        limit: usize,
        all: bool,
        filter: &SwarmFilter,
        older: bool,
    ) -> Result<SwarmFeed> {
        ensure!(
            bound >= 0 && (1..=500).contains(&limit),
            Invalid.error("invalid_page", "invalid swarm cursor or limit")
        );
        let tx = self.conn.unchecked_transaction()?;
        let (a, r) = self.swarm_on(&tx, actor, run, Action::Read)?;
        ensure!(
            matches!(
                filter.kind.as_deref(),
                None | Some("public" | "dm" | "addressed")
            ),
            Invalid.error("invalid_swarm_filter", "expected public, dm or addressed")
        );
        let author = filter
            .worker
            .as_deref()
            .map(|name| {
                (if name == "coordinator" {
                    Some(r.coordinator_actor.as_str())
                } else {
                    r.workers
                        .iter()
                        .find(|w| w.name == name)
                        .map(|w| w.actor.as_str())
                })
                .context(Missing.error("worker_not_found", "unknown filter worker"))
            })
            .transpose()?;
        let op = if older { "<" } else { ">" };
        let order = if older { "DESC" } else { "ASC" };
        // An intended recipient remains an addressee even if the peer wake budget was exhausted.
        let addressed = |actor: &str, worker: &str| {
            format!("(
                EXISTS(SELECT 1 FROM exact_mentions x WHERE x.message_id=messages.id AND x.actor={actor})
                OR EXISTS(SELECT 1 FROM swarm_notifications n WHERE n.message_id=messages.id AND n.target_worker={worker})
                OR EXISTS(SELECT 1 FROM swarm_evidence e LEFT JOIN swarm_evidence parent
                    ON parent.run_id=e.run_id AND parent.message_id=coalesce(json_extract(e.payload,'$.handoff'),json_extract(e.payload,'$.request'))
                    WHERE e.message_id=messages.id AND CASE e.kind
                        WHEN 'decision-request' THEN json_extract(e.payload,'$.to')={actor}
                        WHEN 'handoff' THEN json_extract(e.payload,'$.to')={worker}
                        ELSE parent.actor={actor} END))")
        };
        let mine = addressed("?6", "?7");
        let member = addressed("?11", "?10");
        let sql=format!("SELECT id AS cursor,0 AS dm,id FROM messages WHERE channel=?1 AND id>=?2 AND id{op}?3
            AND (?9 IS NULL OR ?9='public' OR (?9='addressed' AND {mine}))
            AND (?10 IS NULL OR actor=?11 OR {member})
            UNION ALL SELECT event_cursor,1,id FROM swarm_dms WHERE run_id=?4 AND event_cursor{op}?3 AND (?5 OR from_actor=?6 OR to_worker=?7)
            AND (?9 IS NULL OR ?9='dm' OR (?9='addressed' AND to_worker=?7)) AND (?10 IS NULL OR from_actor=?11 OR to_worker=?10)
            ORDER BY cursor {order} LIMIT ?8");
        let mut batch = rows(tx.prepare(&sql)?.query_map(
            params![
                r.channel,
                r.root_id,
                bound,
                r.id,
                r.can_see_dm(actor, &a, all),
                actor,
                r.name_for(actor),
                limit as i64,
                filter.kind,
                filter.worker,
                author
            ],
            |s| {
                Ok((
                    s.get::<_, i64>(0)?,
                    s.get::<_, bool>(1)?,
                    s.get::<_, i64>(2)?,
                ))
            },
        )?)?;
        if older {
            batch.reverse();
        }
        let mut entries = Vec::with_capacity(batch.len());
        for (cursor, is_dm, id) in batch {
            let (message, dm) = if is_dm {
                (None, Some(dm_on(&tx, id)?))
            } else {
                (Some(a.view(&tx, Board::get_on(&tx, id)?)?), None)
            };
            entries.push(SwarmEntry {
                cursor,
                kind: if is_dm { "dm" } else { "public" }.into(),
                notified: message
                    .as_ref()
                    .map(|m| m.notified.clone())
                    .unwrap_or_default(),
                suppressed: message
                    .as_ref()
                    .map(|m| m.suppressed.clone())
                    .unwrap_or_default(),
                message,
                dm,
            });
        }
        let cursor = entries
            .last()
            .map_or(if older { 0 } else { bound }, |e| e.cursor);
        tx.commit()?;
        Ok(SwarmFeed { entries, cursor })
    }
}
