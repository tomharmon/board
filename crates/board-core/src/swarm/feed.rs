use super::*;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SwarmFilter {
    pub kind: Option<String>,
    pub worker: Option<String>,
}

pub(super) fn dm_on(c: &Connection, id: i64) -> Result<SwarmDm> {
    Ok(c.query_row("SELECT id,run_id,from_actor,to_worker,text,event_cursor,created_at FROM swarm_dms WHERE id=?",[id],|r|Ok(SwarmDm{id:r.get(0)?,run_id:r.get(1)?,from_actor:r.get(2)?,to_worker:r.get(3)?,text:r.get(4)?,event_cursor:r.get(5)?,created_at:r.get(6)?}))?)
}
fn notified_on(c: &Connection, id: i64) -> Result<Vec<String>> {
    rows(c.prepare("SELECT target_worker FROM swarm_notifications WHERE message_id=? AND source_kind='mention' ORDER BY target_worker")?.query_map([id],|r|r.get(0))?)
}
fn notify(
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
fn spend(tx: &Transaction, run: &SwarmRun, actor: &str, n: i64) -> Result<()> {
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
        a.files(tx, actor, &opts.attachment_ids)?;
        if !opts.labels.is_empty() {
            a.require(Action::Label, &r.channel)?;
        }
        let (within, mut hits) = resolve(r, actor, text);
        let (body, resolutions) = crate::actors::settle(tx, a, actor, None, &within)?;
        for resolution in &resolutions {
            if let Some(person) = &resolution.actor {
                if let Some(w) = r
                    .workers
                    .iter()
                    .find(|w| w.actor == person.id && w.actor != actor && live(w))
                {
                    hits.insert(w.name.clone());
                }
                if r.coordinator_actor == person.id && actor != person.id {
                    hits.insert("coordinator".into());
                }
            }
        }
        let (m, created) = Board::post_tx_with_outcome(tx, &r.channel, actor, &body, opts)?;
        let mut suppressed = vec![];
        let mut used = 0;
        if created {
            if let Some(target) = super::coordination::record_on(tx, a, r, &m)? {
                hits.insert(target);
            }
            tx.execute(
                "UPDATE swarm_workers SET last_activity_at=? WHERE run_id=? AND actor=?",
                params![crate::now(), r.id, actor],
            )?;
            for who in hits {
                let peer = who != "coordinator" && !r.can_control;
                if peer && r.notifications_sent + used >= r.max_notifications {
                    suppressed.push(who);
                    continue;
                }
                notify(tx, r, actor, &who, "mention", m.id, Some(m.id))?;
                if peer {
                    used += 1;
                }
            }
            spend(tx, r, actor, used)?;
        }
        let notified = notified_on(tx, m.id)?;
        Ok(SwarmPostResult {
            message: crate::actors::write_view(a.view(tx, m)?, &body, resolutions, false),
            notified,
            suppressed,
        })
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
        self.swarm_entries(
            actor,
            run,
            after,
            limit,
            include_dms_for_all,
            &SwarmFilter::default(),
            false,
        )
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
            matches!(filter.kind.as_deref(), None | Some("public" | "dm")),
            Invalid.error("invalid_swarm_filter", "expected public or dm")
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
        let sql=format!("SELECT id AS cursor,0 AS dm,id FROM messages WHERE channel=?1 AND id>=?2 AND id{op}?3
            AND (?9 IS NULL OR ?9='public') AND (?10 IS NULL OR actor=?11 OR EXISTS(SELECT 1 FROM swarm_notifications n WHERE n.message_id=messages.id AND n.target_worker=?10))
            UNION ALL SELECT event_cursor,1,id FROM swarm_dms WHERE run_id=?4 AND event_cursor{op}?3 AND (?5 OR from_actor=?6 OR to_worker=?7)
            AND (?9 IS NULL OR ?9='dm') AND (?10 IS NULL OR from_actor=?11 OR to_worker=?10)
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
            let (message, dm, notified) = if is_dm {
                (None, Some(dm_on(&tx, id)?), vec![])
            } else {
                (
                    Some(a.view(&tx, Board::get_on(&tx, id)?)?),
                    None,
                    notified_on(&tx, id)?,
                )
            };
            entries.push(SwarmEntry {
                cursor,
                kind: if is_dm { "dm" } else { "public" }.into(),
                message,
                dm,
                notified,
            });
        }
        let cursor = entries
            .last()
            .map_or(if older { 0 } else { bound }, |e| e.cursor);
        tx.commit()?;
        Ok(SwarmFeed { entries, cursor })
    }
}
