use super::*;

#[derive(Clone, Copy)]
enum Edge {
    Existing(i64),
    Item(usize),
}
struct Plan<'a> {
    items: &'a [PlanItem],
    edges: Vec<Vec<Edge>>,
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
    let mut adopted = BTreeMap::new();
    for (i, item) in items.iter().enumerate() {
        ensure!(
            item.text.is_some() != item.thread_id.is_some(),
            Invalid.error(
                "invalid_plan_item",
                "provide exactly one of text or thread_id"
            )
        );
        if let Some(id) = item.thread_id {
            ensure!(
                id > 0 && adopted.insert(id, i).is_none(),
                Invalid.error(
                    "invalid_plan_item",
                    "thread_id must be positive and occur only once in the plan"
                )
            );
            ensure!(
                item.attachment_ids.is_empty() && !item.no_sync,
                Invalid.error(
                    "invalid_plan_item",
                    "attachments and no_sync apply only to new threads"
                )
            );
        }
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
                Prerequisite::Key(key) => Edge::Item(*keys.get(key).ok_or_else(bad)?),
                Prerequisite::Id(id) => Edge::Existing(*id),
            };
            // Adopted roots already have stable IDs. A reference to one is a local
            // edge and must participate in cycle checks.
            let edge = match edge {
                Edge::Existing(id) if adopted.contains_key(&id) => Edge::Item(adopted[&id]),
                other => other,
            };
            if let Edge::Item(j) = edge {
                counts[i] += 1;
                dependents[j].push(i);
            }
            prior.push(edge);
        }
        edges.push(prior);
    }
    let mut ready: Vec<_> = counts
        .iter()
        .enumerate()
        .filter_map(|(i, n)| (*n == 0).then_some(i))
        .collect();
    let mut visited = 0;
    while let Some(i) = ready.pop() {
        visited += 1;
        for &j in &dependents[i] {
            counts[j] -= 1;
            if counts[j] == 0 {
                ready.push(j);
            }
        }
    }
    ensure!(visited == items.len(), bad());
    Ok(Plan { items, edges })
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

impl Plan<'_> {
    fn publish(
        &self,
        tx: &Transaction<'_>,
        access: &Access,
        actor: &str,
        channel: &str,
    ) -> Result<Vec<Task>> {
        let mut posted = Vec::with_capacity(self.items.len());
        for item in self.items {
            let (id, mentions) = if let Some(id) = item.thread_id {
                access.message(tx, id, Action::Task)?;
                require_adoptable(tx, id)?;
                (id, vec![])
            } else {
                access.files(tx, actor, &item.attachment_ids)?;
                let (text, mentions) =
                    crate::actors::settle(tx, access, actor, None, item.text.as_deref().unwrap())?;
                let root = Board::post_tx(
                    tx,
                    channel,
                    actor,
                    &text,
                    &PostOptions {
                        attachment_ids: item.attachment_ids.clone(),
                        no_sync: item.no_sync,
                        ..Default::default()
                    },
                )?;
                (root.id, mentions)
            };
            let assignee = plan_assignee(actor, item.assignee.as_deref())?;
            tx.execute(
                "INSERT INTO tasks(id,owner,assignee,state) VALUES(?,?,?,'ready')",
                params![id, actor, assignee],
            )?;
            posted.push((id, mentions));
        }
        let ids: Vec<_> = posted.iter().map(|(id, _)| *id).collect();
        let mut tasks = Vec::with_capacity(ids.len());
        for (i, (id, mentions)) in posted.into_iter().enumerate() {
            let mut needs = Vec::new();
            let mut external = Vec::new();
            for &edge in &self.edges[i] {
                needs.push(match edge {
                    Edge::Item(j) => {
                        let id = ids[j];
                        if self.items[j].thread_id.is_some() {
                            external.push(id);
                        }
                        id
                    }
                    Edge::Existing(id) => {
                        ensure!(
                            !ids.contains(&id),
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
            set_state(tx, id, initial_readiness(&needs), None)?;
            let mut task = publish_transition(tx, id, actor, "created")?;
            remember_creation(tx, access, actor, &task)?;
            task.mentions = mentions;
            tasks.push(task);
        }
        Ok(tasks)
    }
}

pub(super) fn create(
    board: &Board,
    actor: &str,
    channel: &str,
    items: &[PlanItem],
    key: Option<&str>,
    run: Option<&str>,
) -> Result<(Vec<Task>, bool)> {
    if let Some(key) = key {
        crate::core::validate_client_key(key)?;
    }
    let plan = parse(items)?;
    let tx = write(&board.conn)?;
    let a = board.access_on(&tx)?;
    a.actor(actor)?;
    let swarm = run
        .map(|r| board.swarm_for_plan(&tx, actor, r))
        .transpose()?;
    if items.iter().any(|i| i.text.is_some()) {
        a.require(Action::Post, channel)?;
    }
    let receipt = if let Some(key) = key {
        crate::automation::authorize_callback_key(&tx, actor, key)?;
        let digest = fingerprint(("plan", channel, swarm.as_ref().map(|s| s.id), items))?;
        if let Some(r) = batch_receipt(&tx, key)? {
            ensure!(r.actor == actor && r.fingerprint == digest, duplicate());
            let tasks = batch_tasks(&tx, &r)?
                .into_iter()
                .map(|t| creation_view(&tx, &a, actor, t))
                .collect::<Result<Vec<_>>>()?;
            tx.commit()?;
            return Ok((tasks, false));
        }
        Some((key, digest))
    } else {
        None
    };
    if let Some(s) = &swarm {
        s.active()?;
    }
    let tasks = plan.publish(&tx, &a, actor, channel)?;
    if let Some(s) = swarm {
        for task in &tasks {
            tx.execute(
                "INSERT INTO swarm_plan_tasks(run_id,thread_id) VALUES(?,?)",
                params![s.id, task.id],
            )?;
        }
    }
    if let Some((key, digest)) = receipt {
        let ids: Vec<_> = tasks.iter().map(|t| t.id).collect();
        tx.execute(
            "INSERT INTO client_batches(key,actor,fingerprint,ids) VALUES(?,?,?,?)",
            params![key, actor, digest, serde_json::to_string(&ids)?],
        )?;
    }
    tx.commit()?;
    Ok((tasks, true))
}
