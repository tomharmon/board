mod catalog;
pub(crate) mod wait;
pub(crate) use catalog::{catalog, find};

use anyhow::{Context, Result, bail, ensure};
use board_local::{
    Board, Filter, PostOptions, automation as work,
    swarm::{RunPolicy, SwarmAddWorker, SwarmStart},
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

fn selected_run(ctx: &ActorContext, p: &Value) -> Result<String> {
    if let Some(binding) = &ctx.worker {
        return binding.select_run(reference(&p["run"]).as_deref());
    }
    reference(&p["run"])
        .or_else(|| ctx.default_run.clone())
        .context("set run or BOARD_SWARM_RUN")
}

fn claim(b: &Board, ctx: &mut ActorContext, p: &Value) -> Result<Value> {
    let target = match (p["id"].as_i64(), p["ready_channel"].as_str()) {
        (Some(id), None) if p.get("wait_seconds").is_none() => work::ClaimTarget::Id(id),
        (None, Some(channel)) => work::ClaimTarget::Ready(channel),
        _ => bail!("supply id or ready_channel; only a ready queue can wait"),
    };
    let labels = p
        .get("labels")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    if let work::ClaimTarget::Ready(scope) = target
        && p["wait_seconds"].as_u64().unwrap_or(0) > 0
        && !work::ready_for_worker(b, &ctx.actor, scope, &labels, ctx.worker.as_ref())?
    {
        return Err(work::NothingReady.into());
    }
    let token = uuid::Uuid::new_v4().to_string();
    let task = work::claim_in_assignment(
        b,
        &ctx.actor,
        target,
        work::ClaimRequest {
            lease_seconds: p["lease_seconds"].as_i64().unwrap_or(300),
            token: &token,
        },
        &labels,
        &[],
        ctx.worker.as_ref(),
    )?;
    remember(ctx, task)
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
    let run = || selected_run(ctx, p);
    let labels: BTreeMap<String, String> = p
        .get("labels")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    let opts = PostOptions {
        labels: labels.clone(),
        no_sync: p["no_sync"].as_bool().unwrap_or(false),
        key: owned("idempotency_key"),
        ..Default::default()
    };
    match tool.name {
        "swarm_start" => value(
            b.swarm_start_with_policy(
                actor,
                &SwarmStart {
                    goal: required("goal")?.into(),
                    slug: owned("slug"),
                    repo_root: owned("repo_root"),
                    max_notifications: p["max_notifications"].as_i64(),
                },
                RunPolicy {
                    max_active: p["max_active"]
                        .as_i64()
                        .unwrap_or(RunPolicy::default().max_active),
                    retry_budget: p["retry_budget"].as_i64().unwrap_or(0),
                },
            )?,
        ),
        "swarm_plan" => value(b.swarm_plan(
            actor,
            &run()?,
            required("key")?,
            &serde_json::from_value::<Vec<_>>(p["items"].clone())?,
        )?),
        "swarm_observe" => value(b.swarm_observe(
            actor,
            &run()?,
            after,
            p["action_after"].as_i64().unwrap_or(0),
            p["limit"].as_u64().unwrap_or(20) as usize,
        )?),
        "swarm_list" => value(b.swarm_list(actor, p["before"].as_i64(), size)?),
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
        "swarm_check_in" => {
            let r = run()?;
            let (target, _) = b.swarm_wait_target(actor, &r)?;
            b.swarm_check_in(actor, &r, &target)?;
            if p["compact"] == true {
                return Ok(json!({"run":r,"worker":target,"actor":actor,"status":"working"}));
            }
            value(b.swarm_status(actor, &r)?)
        }
        "swarm_launch_failed" => {
            let r = run()?;
            b.swarm_launch_failed(actor, &r, required("name")?, required("reason")?)?;
            value(b.swarm_status(actor, &r)?)
        }
        "swarm_receipt" => value(b.swarm_receipt(actor, &run()?)?),
        "board_attention" => value(b.attention(actor)?),
        "swarm_decide" => {
            value(b.swarm_resolve_decision(actor, &run()?, id()?, required("summary")?)?)
        }
        "swarm_set_limit" => value(
            b.swarm_set_limit(
                actor,
                &run()?,
                p["max_notifications"]
                    .as_i64()
                    .context("missing max_notifications")?,
            )?,
        ),
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
            let (cursor, mut tasks) =
                work::filtered_tasks_page_as(b, Some(actor), &f, after.unwrap_or(0), size + 1)?;
            let more = tasks.len() > size;
            tasks.truncate(size);
            Ok(
                json!({"cursor":cursor,"tasks":tasks,"next_after": tasks.last().filter(|_|more).map(|t|t.id)}),
            )
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
        "tasks_claim" => claim(b, ctx, p),
        "tasks_complete" | "tasks_fail" | "tasks_block" | "tasks_renew" | "tasks_release" => {
            let id = id()?;
            let (epoch, token) = ctx
                .leases
                .get(&id)
                .map(|(epoch, token)| (Some(*epoch), Some(token.as_str())))
                .unwrap_or_default();
            let action = match tool.name {
                "tasks_complete" => work::TaskAction::Complete(s("result")),
                "tasks_fail" => work::TaskAction::Fail(required("reason")?),
                "tasks_renew" => {
                    work::TaskAction::Renew(p["lease_seconds"].as_i64().unwrap_or(300))
                }
                "tasks_release" => work::TaskAction::Release,
                _ => work::TaskAction::Block(required("reason")?),
            };
            let result = work::change_task(b, id, actor, epoch, token, action);
            if (result.is_ok() && tool.name != "tasks_renew")
                || result
                    .as_ref()
                    .is_err_and(|e| board_local::errors::classify(e).1 == "claim_lost")
            {
                ctx.leases.remove(&id);
            }
            remember(ctx, result?)
        }
        _ => bail!("unimplemented tool {}", tool.name),
    }
}
