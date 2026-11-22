use super::*;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Serialize)]
enum Edge {
    Existing(i64),
    Item(usize),
}
struct Graph {
    edges: Vec<Vec<Edge>>,
    order: Vec<usize>,
    digests: Vec<Vec<u8>>,
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

fn graph(channel: &str, items: &[PlanItem]) -> Result<Graph> {
    let mut keys = BTreeMap::new();
    for (i, item) in items.iter().enumerate() {
        if let Some(k) = &item.key {
            ensure!(!k.is_empty() && keys.insert(k, i).is_none(), bad());
        }
    }
    let mut edges = Vec::new();
    let mut counts = vec![0; items.len()];
    let mut dependents = vec![Vec::new(); items.len()];
    let mut digests = Vec::new();
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
        digests.push(fingerprint((channel, item, &prior))?);
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
    Ok(Graph {
        edges,
        order,
        digests,
    })
}
pub(super) fn validate(channel: &str, items: &[PlanItem]) -> Result<()> {
    graph(channel, items).map(|_| ())
}

pub(super) fn create(
    board: &Board,
    actor: &str,
    channel: &str,
    items: &[PlanItem],
    key: &str,
    guard: &impl Fn(&Transaction<'_>) -> Result<()>,
) -> Result<(Vec<Task>, bool)> {
    crate::core::validate_client_key(key)?;
    let g = graph(channel, items)?;
    let full = fingerprint(("plan", channel, items))?;
    let partial = fingerprint(("unfinished", channel))?;
    let mut made = false;
    loop {
        // Commit the checkpoint with the messages. A crash cannot leave a chunk unaccounted for.
        let tx = write(&board.conn)?;
        guard(&tx)?;
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
            ensure!(g.digests.get(i) == Some(&recorded), duplicate());
            known.insert(i, id);
        }
        let indices: Vec<_> = g
            .order
            .iter()
            .copied()
            .filter(|i| !known.contains_key(i))
            .take(100)
            .collect();
        let chunk = indices
            .iter()
            .map(|&i| {
                let mut item = items[i].clone();
                item.key = Some(i.to_string());
                item.needs = g.edges[i]
                    .iter()
                    .map(|e| match e {
                        Edge::Existing(id) => Prerequisite::Number(*id),
                        Edge::Item(j) => Prerequisite::Key { key: j.to_string() },
                    })
                    .collect();
                item
            })
            .collect::<Vec<_>>();
        for (i, t) in indices.into_iter().zip(plan_chunk(
            &tx,
            &a,
            actor,
            channel,
            &chunk,
            known.iter().map(|(i, id)| (i.to_string(), *id)).collect(),
        )?) {
            tx.execute(
                "INSERT INTO plan_progress VALUES(?,?,?,?)",
                params![key, i as i64, g.digests[i], t.id],
            )?;
            known.insert(i, t.id);
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
