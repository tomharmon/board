use super::*;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Serialize)]
enum Edge {
    Existing(i64),
    Item(usize),
}
struct Plan<'a> {
    items: &'a [PlanItem],
    edges: Vec<Vec<Edge>>,
    order: Vec<usize>,
}
fn bad() -> crate::errors::Rejection {
    Invalid.error("invalid_needs", "invalid or cyclic plan prerequisites")
}
fn duplicate() -> crate::errors::Rejection {
    Conflict.error(
        "duplicate_id",
        "plan key conflicts with work already committed",
    )
}

fn parse(items: &[PlanItem]) -> Result<Plan<'_>> {
    let mut keys = BTreeMap::new();
    for (i, item) in items.iter().enumerate() {
        if let Some(k) = &item.key {
            ensure!(!k.is_empty() && keys.insert(k, i).is_none(), bad());
        }
    }
    let mut edges = Vec::new();
    let mut counts = vec![0; items.len()];
    let mut dependents = vec![Vec::new(); items.len()];
    for (i, item) in items.iter().enumerate() {
        let mut prior = Vec::new();
        for p in &item.needs {
            let edge = match p {
                Prerequisite::Name(s) => match keys.get(s) {
                    Some(i) => Edge::Item(*i),
                    None => Edge::Existing(s.parse().map_err(|_| bad())?),
                },
                Prerequisite::Number(id) => Edge::Existing(*id),
                Prerequisite::Key { key } => Edge::Item(*keys.get(key).ok_or_else(bad)?),
                Prerequisite::Id { id } => Edge::Existing(
                    id.as_i64()
                        .or_else(|| id.as_str().and_then(|s| s.parse().ok()))
                        .ok_or_else(bad)?,
                ),
            };
            if let Edge::Item(j) = edge {
                counts[i] += 1;
                dependents[j].push(i);
            }
            prior.push(edge);
        }
        edges.push(prior);
    }
    let mut ready: BTreeSet<_> = counts
        .iter()
        .enumerate()
        .filter_map(|(i, n)| (*n == 0).then_some(i))
        .collect();
    let mut order = Vec::new();
    while let Some(i) = ready.pop_first() {
        order.push(i);
        for &j in &dependents[i] {
            counts[j] -= 1;
            if counts[j] == 0 {
                ready.insert(j);
            }
        }
    }
    ensure!(order.len() == items.len(), bad());
    Ok(Plan {
        items,
        edges,
        order,
    })
}

fn plan_assignee(actor: &str, assignee: Option<&str>) -> Result<Option<String>> {
    let who = assignee.map(|p| {
        if p.contains('/') {
            p.to_owned()
        } else {
            format!("{}/{}", person(actor), p)
        }
    });
    if let Some(who) = who.as_deref() {
        validate_assignee(actor, who)?;
    }
    Ok(who)
}

pub(super) fn publish_all(
    tx: &Transaction<'_>,
    access: &Access,
    actor: &str,
    channel: &str,
    items: &[PlanItem],
) -> Result<Vec<Task>> {
    // Atomic plans preserve input order, including when a dependency is listed later.
    let indices = (0..items.len()).collect::<Vec<_>>();
    parse(items)?.publish(tx, access, actor, channel, &indices, &mut BTreeMap::new())
}

impl Plan<'_> {
    fn publish(
        &self,
        tx: &Transaction<'_>,
        access: &Access,
        actor: &str,
        channel: &str,
        indices: &[usize],
        known: &mut BTreeMap<usize, i64>,
    ) -> Result<Vec<Task>> {
        let mut posted = Vec::with_capacity(indices.len());
        for &i in indices {
            let item = &self.items[i];
            access.files(tx, actor, &item.attachment_ids)?;
            let (text, mentions) = crate::actors::settle(tx, access, actor, None, &item.text)?;
            let id = Board::post_tx(
                tx,
                channel,
                actor,
                &text,
                &PostOptions {
                    labels: item.labels.clone(),
                    attachment_ids: item.attachment_ids.clone(),
                    no_sync: item.no_sync,
                    ..Default::default()
                },
            )?
            .id;
            let assignee = plan_assignee(actor, item.assignee.as_deref())?;
            tx.execute(
                "INSERT INTO tasks(id,owner,assignee,state) VALUES(?,?,?,'ready')",
                params![id, actor, assignee],
            )?;
            known.insert(i, id);
            posted.push((id, mentions));
        }
        let new_ids: BTreeSet<_> = posted.iter().map(|(id, _)| *id).collect();
        let mut tasks = Vec::with_capacity(indices.len());
        for (&i, (id, mentions)) in indices.iter().zip(posted) {
            let mut needs = Vec::new();
            let mut external = Vec::new();
            for &edge in &self.edges[i] {
                needs.push(match edge {
                    Edge::Item(j) => known[&j],
                    Edge::Existing(id) => {
                        ensure!(
                            !new_ids.contains(&id),
                            Invalid.error(
                                "invalid_needs",
                                "new plan items must be named by their plan keys"
                            )
                        );
                        external.push(id);
                        id
                    }
                });
            }
            let needs = distinct_needs(&needs)?;
            visible_needs(access, tx, &external)?;
            replace_needs(tx, id, &needs)?;
            set_state(tx, id, queued(&needs), None)?;
            let mut task = publish_queue_state(tx, id, actor, "created")?;
            remember_creation(tx, access, actor, &task)?;
            task.mentions = mentions;
            tasks.push(task);
        }
        Ok(tasks)
    }
}

pub(super) fn create_checkpointed(
    board: &Board,
    actor: &str,
    channel: &str,
    items: &[PlanItem],
    key: &str,
) -> Result<(Vec<Task>, bool)> {
    crate::core::validate_client_key(key)?;
    let plan = parse(items)?;
    let digests = items
        .iter()
        .zip(&plan.edges)
        .map(|(item, edges)| fingerprint((channel, item, edges)))
        .collect::<Result<Vec<_>>>()?;
    let full = fingerprint(("plan", channel, items))?;
    let partial = fingerprint(("unfinished", channel))?;
    let mut made = false;
    loop {
        // Commit the checkpoint with the messages. A crash cannot leave a chunk unaccounted for.
        let tx = write(&board.conn)?;
        let a = board.access_on(&tx)?;
        a.actor(actor)?;
        crate::automation::authorize_callback_key(&tx, actor, key)?;
        a.require(Action::Post, channel)?;
        if items.iter().any(|i| !i.labels.is_empty()) {
            a.require(Action::Label, channel)?;
        }
        if let Some(r) = batch_receipt(&tx, key)? {
            ensure!(r.actor == actor, duplicate());
            if r.fingerprint == full {
                let tasks = batch_tasks(&tx, &r)?
                    .into_iter()
                    .map(|t| creation_view(&tx, &a, actor, t))
                    .collect::<Result<Vec<_>>>()?;
                tx.commit()?;
                return Ok((tasks, made));
            }
            ensure!(r.fingerprint == partial, duplicate());
        }
        let mut known = BTreeMap::new();
        for row in tx
            .prepare(
                "SELECT position,digest,task FROM plan_progress WHERE key=? ORDER BY position",
            )?
            .query_map([key], |r| {
                Ok((
                    r.get::<_, usize>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
        {
            let (i, recorded, id) = row?;
            ensure!(digests.get(i) == Some(&recorded), duplicate());
            known.insert(i, id);
        }
        let indices: Vec<_> = plan
            .order
            .iter()
            .copied()
            .filter(|i| !known.contains_key(i))
            .take(PLAN_CHUNK_SIZE)
            .collect();
        plan.publish(&tx, &a, actor, channel, &indices, &mut known)?;
        for i in indices {
            tx.execute(
                "INSERT INTO plan_progress VALUES(?,?,?,?)",
                params![key, i as i64, digests[i], known[&i]],
            )?;
            made = true;
        }
        let ids: Vec<_> = known.values().copied().collect();
        let finished = ids.len() == items.len();
        tx.execute(
            "INSERT INTO client_batches(key,actor,fingerprint,ids) VALUES(?,?,?,?)
          ON CONFLICT(key) DO UPDATE SET fingerprint=excluded.fingerprint,ids=excluded.ids",
            params![
                key,
                actor,
                if finished { &full } else { &partial },
                serde_json::to_string(&ids)?
            ],
        )?;
        if finished {
            tx.execute("DELETE FROM plan_progress WHERE key=?", [key])?;
            let out = ids
                .into_iter()
                .map(|id| creation_view(&tx, &a, actor, task(&tx, id)?))
                .collect::<Result<Vec<_>>>()?;
            tx.commit()?;
            return Ok((out, made));
        }
        tx.commit()?;
    }
}
