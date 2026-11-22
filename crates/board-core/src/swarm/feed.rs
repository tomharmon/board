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
pub(crate) fn read_mention_routing(
    c: &Connection,
    a: &Access,
    m: &mut Message,
    run: Option<i64>,
) -> Result<()> {
    let mut stmt = c.prepare_cached(
        "SELECT n.target_worker AS target, 1,r.channel FROM swarm_notifications n JOIN swarm_runs r ON r.id=n.run_id
            WHERE n.message_id=?1 AND n.source_kind='mention' AND (?2 IS NULL OR n.run_id=?2)
         UNION ALL SELECT json_extract(e.payload,'$.target'), 0,r.channel FROM events e JOIN swarm_runs r ON r.slug=json_extract(e.payload,'$.run')
            WHERE e.message_id=?1 AND e.kind='swarm.notification_suppressed' AND (?2 IS NULL OR r.id=?2)
         ORDER BY target",
    )?;
    let mut rows = stmt.query(params![m.id, run])?;
    m.notified.clear();
    m.suppressed.clear();
    while let Some(row) = rows.next()? {
        if !a.allows(Action::Read, &row.get::<_, String>(2)?) {
            continue;
        }
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
            Some(run.thread_id),
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
        self.swarm_post_to_thread(actor, run, None, text, opts)
    }
    pub fn swarm_post_to_thread(
        &self,
        actor: &str,
        run: &str,
        reply_to: Option<i64>,
        text: &str,
        opts: &PostOptions,
    ) -> Result<SwarmPostResult> {
        self.write(|tx| {
            let (a, r) = self.swarm_on(tx, actor, run, Action::Post)?;
            r.active()?;
            let id=reply_to.unwrap_or(r.thread_id);
            if id!=r.thread_id {
                a.message(tx,id,Action::Post)?;
                ensure!(tx.query_row("SELECT EXISTS(SELECT 1 FROM swarm_threads WHERE run_id=? AND thread_id=(SELECT coalesce(re,id) FROM messages WHERE id=?))", params![r.id,id],|s|s.get::<_,bool>(0))?,
                    Invalid.error("unlinked_thread","thread is not linked to this swarm"));
            }
            Ok(self.publish_public_on(tx,&a,actor,PublicDestination::Run(&r,id),text,opts)?.0)
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
            .publish_public_on(
                tx,
                a,
                actor,
                PublicDestination::Run(r, r.thread_id),
                text,
                opts,
            )?
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
            PublicDestination::Run(r, id) => {
                reply_channel = a.message(tx, id, Action::Post)?;
                (reply_channel.as_str(), Some(id), Some(r))
            }
        };
        a.require(Action::Post, channel)?;
        a.files(tx, actor, &opts.attachment_ids)?;
        let blind = !a.allows(Action::Read, channel);
        // A board reply can use the roster only when its active run is unambiguous.
        // A top-level channel post never belongs to a swarm just by proximity.
        let loaded = if let (None, Some(id)) = (known_run, parent) {
            let mut stmt=tx.prepare_cached("SELECT r.slug,r.channel FROM swarm_runs r JOIN swarm_threads s ON s.run_id=r.id
                WHERE s.thread_id=(SELECT coalesce(re,id) FROM messages WHERE id=?1) AND r.status='active'")?;
            let mut candidates = stmt.query([id])?;
            let mut slug = None;
            while let Some(row) = candidates.next()? {
                let anchor: String = row.get(1)?;
                if a.allows(Action::Read, &anchor) && a.allows(Action::Post, &anchor) {
                    if slug.is_some() {
                        slug = None;
                        break;
                    }
                    slug = Some(row.get::<_, String>(0)?);
                }
            }
            slug.map(|s| self.swarm_on(tx, actor, &s, Action::Post).map(|(_, r)| r))
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
        if let Some(key) = opts.key.as_ref() {
            if created && let Some(r) = run {
                tx.execute(
                    "UPDATE client_posts SET swarm_run_id=? WHERE key=?",
                    params![r.id, key],
                )?;
            } else if !created && let Some(r) = known_run {
                ensure!(
                    tx.query_row(
                        "SELECT swarm_run_id IS ? FROM client_posts WHERE key=?",
                        params![r.id, key],
                        |s| s.get::<_, bool>(0)
                    )?,
                    Conflict.error(
                        "duplicate_id",
                        "client key belongs to a different publication"
                    )
                );
            }
        }
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
            // An ordinary board retry recovers the original publication receipt;
            // today's links (or active rosters) need not match yesterday's.
            let receipt_run = if created { run } else { known_run };
            a.view_in_run(tx, m, receipt_run.map(|r| r.id))?
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
                OR EXISTS(SELECT 1 FROM swarm_notifications n WHERE n.run_id=:run AND n.message_id=messages.id AND n.target_worker={worker})
                OR EXISTS(SELECT 1 FROM swarm_evidence e LEFT JOIN swarm_evidence parent
                    ON parent.run_id=e.run_id AND parent.message_id=coalesce(json_extract(e.payload,'$.handoff'),json_extract(e.payload,'$.request'))
                    WHERE e.run_id=:run AND e.message_id=messages.id AND CASE e.kind
                        WHEN 'decision-request' THEN json_extract(e.payload,'$.to')={actor}
                        WHEN 'handoff' THEN json_extract(e.payload,'$.to')={worker}
                        ELSE parent.actor={actor} END))")
        };
        let mine = addressed(":me", ":name");
        let member = addressed(":author", ":worker");
        let sql=format!("SELECT id AS cursor,0 AS dm,id FROM messages WHERE coalesce(re,id) IN
            (SELECT thread_id FROM swarm_threads WHERE run_id=:run) AND (:channels IS NULL OR channel IN (SELECT value FROM json_each(:channels))) AND id{op}:bound
            AND (:kind IS NULL OR :kind='public' OR (:kind='addressed' AND {mine}))
            AND (:worker IS NULL OR actor=:author OR {member})
            UNION ALL SELECT event_cursor,1,id FROM swarm_dms WHERE run_id=:run AND event_cursor{op}:bound AND (:all OR from_actor=:me OR to_worker=:name)
            AND (:kind IS NULL OR :kind='dm' OR (:kind='addressed' AND to_worker=:name)) AND (:worker IS NULL OR from_actor=:author OR to_worker=:worker)
            ORDER BY cursor {order} LIMIT :limit");
        let mut batch = rows(tx.prepare(&sql)?.query_map(
            rusqlite::named_params!{
                ":channels": a.read_channels_json(&tx)?, ":bound": bound, ":run": r.id,
                ":all": r.can_see_dm(actor, &a, all), ":me": actor, ":name": r.name_for(actor),
                ":limit": limit as i64, ":kind": filter.kind, ":worker": filter.worker, ":author": author
            },
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
                (
                    Some(a.view_in_run(&tx, Board::get_on(&tx, id)?, Some(r.id))?),
                    None,
                )
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
