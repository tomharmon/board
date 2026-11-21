mod catalog;
mod wait;
pub(crate) use catalog::{catalog, find};

use anyhow::{Context, Result, bail, ensure};
use board_local::{
    Board, Filter, PostOptions, automation as work,
    swarm::{SwarmAddWorker, SwarmStart},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::ActorContext;

fn value<T: Serialize>(data: T) -> Result<Value> {
    Ok(serde_json::to_value(data)?)
}
fn reference(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::to_owned)
        .or_else(|| v.as_i64().map(|i| format!("#{i}")))
}
fn remember(ctx: &mut ActorContext, mut task: work::Task) -> Result<Value> {
    if let Some(proof) = task.claim_token.take() {
        ctx.leases.insert(task.id, (task.epoch, proof));
    }
    value(task)
}

pub(crate) fn execute(
    b: &Board,
    ctx: &mut ActorContext,
    tool: &catalog::Tool,
    p: &Value,
) -> Result<Value> {
    tool.validate(p)?;
    if ctx.worker.is_some() && catalog::coordinator_tool(tool.name) {
        return Err(board_local::swarm::WorkerBinding::coordinator_only());
    }
    let actor = ctx.actor.as_str();
    let s = |k: &str| p[k].as_str();
    let required = |k: &str| s(k).with_context(|| format!("missing {k}"));
    let owned = |k: &str| s(k).map(str::to_owned);
    let id = || p["id"].as_i64().context("missing task or message ID");
    let after = p["after"].as_i64();
    let size = p["limit"].as_u64().unwrap_or(50) as usize;
    let run = || {
        if let Some(binding) = &ctx.worker {
            return binding.select_run(reference(&p["run"]).as_deref());
        }
        reference(&p["run"])
            .or_else(|| ctx.default_run.clone())
            .context("set run or BOARD_SWARM_RUN")
    };
    let labels: BTreeMap<String, String> = p
        .get("labels")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    let opts = PostOptions {
        labels: labels.clone(),
        no_sync: p["no_sync"].as_bool().unwrap_or(false),
        ..Default::default()
    };
    match tool.name {
        "swarm_start" => value(b.swarm_start(
            actor,
            &SwarmStart {
                goal: required("goal")?.into(),
                slug: owned("slug"),
                repo_root: owned("repo_root"),
                max_notifications: p["max_notifications"].as_i64(),
            },
        )?),
        "swarm_add" => value(b.swarm_add_worker(
            actor,
            &run()?,
            &SwarmAddWorker {
                name: required("name")?.into(),
                brief: required("brief")?.into(),
                role_slug: owned("role"),
                workspace_path: owned("workspace_path"),
                branch_name: owned("branch_name"),
            },
        )?),
        "swarm_status" => value(b.swarm_status(actor, &run()?)?),
        "swarm_post" => {
            let mut text = required("text")?.to_owned();
            match (s("schema"), p.get("schema_data")) {
                (None, None) => {}
                (Some(schema), Some(data)) => {
                    let (kind, version) = schema
                        .rsplit_once('@')
                        .context("schema must be NAME@VERSION")?;
                    let version = version.parse::<u32>().context("invalid schema version")?;
                    b.schema(kind, version)?;
                    text = board_local::append_block(&text, kind, version, data)?;
                }
                _ => bail!("schema and schema_data must be supplied together"),
            }
            value(b.swarm_post(actor, &run()?, &text, &opts)?)
        }
        "swarm_dm" => value(b.swarm_dm(actor, &run()?, required("to")?, required("text")?)?),
        "swarm_read" => value(b.swarm_read(actor, &run()?, after.unwrap_or(0), size, true)?),
        "swarm_wait" => wait::run(
            b,
            actor,
            &run()?,
            after,
            p["timeout_seconds"].as_u64().unwrap_or(90),
        ),
        "swarm_done" => value(b.swarm_done(actor, &run()?, s("summary"))?),
        "swarm_stop" => value(b.swarm_stop(actor, &run()?)?),
        "roles_list" => value(b.roles_list()?),
        "roles_show" => value(b.role_show(required("slug")?)?),
        "board_whoami" => Ok(
            json!({"actor":actor, "worker":ctx.worker, "database":b.database_file(), "default_run":ctx.default_run}),
        ),
        "board_channels" => {
            let (cursor, channels, next) = b.channels_window("", None, s("prefix"), size)?;
            Ok(json!({"cursor":cursor,"channels":channels,"next":next}))
        }
        "board_post" => value(b.post(required("channel")?, actor, required("text")?, &opts)?),
        "board_reply" => value(b.reply(id()?, actor, required("text")?, &opts)?),
        "board_read" => {
            let mut f = Filter {
                channel: owned("channel"),
                text: owned("text"),
                by: owned("by"),
                mentions: owned("mentions"),
                after,
                ..Default::default()
            };
            f.bind_actor(actor);
            if let Some(target) = reference(&p["target"]) {
                if let Ok(id) = target.trim_start_matches('#').parse::<i64>() {
                    ensure!(
                        id > 0 && f.channel.is_none(),
                        "a thread target must be positive and cannot be combined with channel"
                    );
                    return value(b.conversation_window(id, &f, Some(size))?);
                } else {
                    ensure!(
                        f.channel.is_none(),
                        "use a channel target or channel, not both"
                    );
                    f.channel = Some(target);
                }
            }
            value(b.messages_window(&f, None, size)?)
        }
        "tasks_list" => {
            let f = work::TaskFilter {
                channel: owned("channel"),
                state: owned("state"),
                assignee: owned("assignee"),
                labels,
                ..Default::default()
            };
            let (cursor, tasks) = work::filtered_tasks_page_as(b, Some(actor), &f, 0, size)?;
            Ok(json!({"cursor":cursor,"tasks":tasks}))
        }
        "tasks_create" => {
            let target = work::TaskTarget::New {
                channel: required("channel")?,
                text: required("text")?,
                options: &opts,
            };
            let needs: Vec<i64> = p
                .get("needs")
                .cloned()
                .map(serde_json::from_value)
                .transpose()?
                .unwrap_or_default();
            let token = uuid::Uuid::new_v4().to_string();
            let req = p["claim"]
                .as_bool()
                .unwrap_or(false)
                .then_some(work::ClaimRequest {
                    lease_seconds: 300,
                    token: &token,
                });
            let (t, _) = work::create_task_and_claim(b, actor, target, &needs, s("assignee"), req)?;
            remember(ctx, t)
        }
        "tasks_claim" => {
            let target = match (p["id"].as_i64(), s("ready_channel")) {
                (Some(id), None) => work::ClaimTarget::Id(id),
                (None, Some(channel)) => work::ClaimTarget::Ready(channel),
                _ => bail!("supply exactly one of id and ready_channel"),
            };
            let t = work::claim(
                b,
                actor,
                target,
                p["lease_seconds"].as_i64().unwrap_or(300),
                &labels,
            )?;
            remember(ctx, t)
        }
        "tasks_complete" | "tasks_fail" | "tasks_block" => {
            let id = id()?;
            let (epoch, token) = ctx
                .leases
                .get(&id)
                .map(|(epoch, token)| (Some(*epoch), Some(token.as_str())))
                .unwrap_or_default();
            let action = match tool.name {
                "tasks_complete" => work::TaskAction::Complete(s("result")),
                "tasks_fail" => work::TaskAction::Fail(required("reason")?),
                _ => work::TaskAction::Block(required("reason")?),
            };
            let t = work::change_task(b, id, actor, epoch, token, action)?;
            ctx.leases.remove(&id);
            value(t)
        }
        _ => bail!("unimplemented tool {}", tool.name),
    }
}
