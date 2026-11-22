mod catalog;
mod read;
pub(crate) mod wait;
pub(crate) use catalog::{Call, Published, catalog};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use board_local::{
    Board, PostOptions, automation as work,
    swarm::{SwarmAddWorker, SwarmStart},
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::ActorContext;

#[derive(Serialize, schemars::JsonSchema)]
#[serde(untagged)]
#[schemars(extend("type" = "object"))]
pub(crate) enum ToolOutput<T> {
    Success(T),
    Error { code: String, message: String },
}

#[derive(Serialize, schemars::JsonSchema)]
struct PlanReply {
    key: String,
    tasks: Vec<work::Task>,
}

#[derive(Serialize, schemars::JsonSchema)]
#[serde(untagged)]
enum Observation<'a> {
    Overview(&'a board_local::swarm::SwarmObservation),
    Explanation(&'a board_local::swarm::TaskExplanation),
}

fn value<T: Serialize>(data: T) -> Result<Value> {
    Ok(serde_json::to_value(ToolOutput::Success(data))?)
}
fn remember(ctx: &mut ActorContext, mut task: work::Task) -> Result<Value> {
    if let Some(proof) = task.claim_token.take() {
        ctx.leases.insert(task.id, (task.epoch, proof));
    }
    value(task)
}

fn selected_run(ctx: &ActorContext, run: Option<read::Run>) -> Result<String> {
    let run = run.map(read::Run::reference);
    match &ctx.worker {
        Some(w) => w.select_run(run.as_deref()),
        None => run
            .or_else(|| ctx.default_run.clone())
            .context("set run or BOARD_SWARM_RUN"),
    }
}

fn post_text(
    b: &Board,
    text: String,
    schema: Option<String>,
    data: Option<Value>,
) -> Result<String> {
    match (schema, data) {
        (None, None) => Ok(text),
        (Some(schema), Some(data)) => {
            let (kind, version) = schema
                .rsplit_once('@')
                .context("schema must be NAME@VERSION")?;
            let version = version.parse::<u32>().context("invalid schema version")?;
            b.schema(kind, version)?;
            board_local::append_block(&text, kind, version, &data)
        }
        _ => bail!("schema and schema_data must be supplied together"),
    }
}
fn ids(ids: Option<Vec<catalog::Id>>) -> Vec<i64> {
    ids.unwrap_or_default().into_iter().map(|id| id.0).collect()
}

fn change(
    b: &Board,
    ctx: &mut ActorContext,
    id: i64,
    action: work::TaskAction<'_>,
) -> Result<Value> {
    let renewing = matches!(action, work::TaskAction::Renew(_));
    let (epoch, token) = ctx
        .leases
        .get(&id)
        .map(|(epoch, token)| (Some(*epoch), Some(token.as_str())))
        .unwrap_or_default();
    let result = work::change_task(b, id, &ctx.actor, epoch, token, action);
    if (result.is_ok() && !renewing)
        || result
            .as_ref()
            .is_err_and(|e| board_local::errors::classify(e).1 == "claim_lost")
    {
        ctx.leases.remove(&id);
    }
    remember(ctx, result?)
}

pub(crate) fn execute(b: &Board, ctx: &mut ActorContext, call: Call) -> Result<Value> {
    let actor = ctx.actor.as_str();
    let run = |r| selected_run(ctx, r);
    match call {
        Call::SwarmStart(p) => value(b.swarm_start(
            actor,
            &SwarmStart {
                goal: p.goal,
                thread_id: p.thread_id.map(|x| x.0),
                channel: p.channel,
                slug: Some(p.slug),
                repo_root: p.repo_root,
                max_notifications: p.max_notifications.map(|x| x.0),
            },
        )?),
        Call::SwarmObserve(p) => {
            let r = run(p.run)?;
            let limit = p.limit.map_or(20, |x| x.0);
            if let Some(id) = p.explain_task {
                value(Observation::Explanation(
                    &b.swarm_explain_task(actor, &r, id.0, limit)?,
                ))
            } else {
                value(Observation::Overview(&b.swarm_observe_page(
                    actor,
                    &r,
                    p.after.map(|x| x.0),
                    p.action_after.map_or(0, |x| x.0),
                    p.task_after.map_or(0, |x| x.0),
                    limit,
                )?))
            }
        }
        Call::SwarmList(p) => {
            value(b.swarm_summaries(actor, p.before.map(|x| x.0), p.limit.map_or(50, |x| x.0))?)
        }
        Call::SwarmAdd(p) => value(b.swarm_register_worker(
            actor,
            &run(p.run)?,
            &SwarmAddWorker {
                name: p.name,
                brief: p.brief,
                thread_id: p.thread_id.map(|x| x.0),
                role_slug: p.role,
                workspace_path: p.workspace_path,
                branch_name: p.branch_name,
            },
        )?),
        Call::WorkerAssignment(p) => {
            let binding =
                ctx.worker
                    .as_ref()
                    .context(board_local::errors::Kind::Forbidden.error(
                        "worker_binding_required",
                        "use a connection bound to your worker assignment",
                    ))?;
            match (p.skill, p.path) {
                (None, None) => value(b.worker_assignment(binding)?),
                (Some(skill), Some(path)) => value(b.worker_skill_file(binding, &skill, &path)?),
                _ => bail!("supply both skill and path to read a pinned file"),
            }
        }
        Call::SwarmStatus(p) => value(b.swarm_status(actor, &run(p.run)?)?),
        Call::SwarmCheckIn(p) => value(b.swarm_check_in(
            actor,
            &run(p.run)?,
            p.model.as_deref(),
            p.workspace_mode.as_deref(),
            p.session_ref.as_deref(),
        )?),
        Call::SwarmLaunchFailed(p) => {
            let r = run(p.run)?;
            b.swarm_launch_failed(actor, &r, &p.name, &p.reason)?;
            value(b.swarm_status(actor, &r)?)
        }
        Call::SwarmReceipt(p) => value(b.swarm_receipt(actor, &run(p.run)?)?),
        Call::BoardAttention(_) => value(b.attention(actor)?),
        Call::SwarmDecide(p) => {
            value(b.swarm_resolve_decision(actor, &run(p.run)?, p.id.0, &p.summary)?)
        }
        Call::SwarmSetLimit(p) => {
            value(b.swarm_set_limit(actor, &run(p.run)?, p.max_notifications.0)?)
        }
        Call::BoardPost(p) => {
            let text = post_text(b, p.text, p.schema, p.schema_data)?;
            let opts = PostOptions {
                no_sync: p.no_sync.unwrap_or(false),
                key: p.idempotency_key,
                attachment_ids: ids(p.attachment_ids),
                ..Default::default()
            };
            match (p.channel, p.reply_to, p.run) {
                (Some(ch), None, None) => value(b.post(&ch, actor, &text, &opts)?),
                (None, Some(id), Some(r)) => value(
                    b.swarm_post_to_thread(actor, &run(Some(r))?, Some(id.0), &text, &opts)?
                        .message,
                ),
                (None, Some(id), None) => value(b.reply(id.0, actor, &text, &opts)?),
                _ => bail!("supply exactly one of channel or reply_to; run requires reply_to"),
            }
        }
        Call::SwarmPost(p) => {
            let text = post_text(b, p.text, p.schema, p.schema_data)?;
            let opts = PostOptions {
                key: p.idempotency_key,
                attachment_ids: ids(p.attachment_ids),
                ..Default::default()
            };
            value(b.swarm_post_to_thread(
                actor,
                &run(p.run)?,
                p.reply_to.map(|x| x.0),
                &text,
                &opts,
            )?)
        }
        Call::SwarmDm(p) => value(b.swarm_dm(actor, &run(p.run)?, &p.to, &p.text)?),
        Call::SwarmDone(p) => value(b.swarm_done(actor, &run(p.run)?, p.summary.as_deref())?),
        Call::SwarmStop(p) => value(b.swarm_stop(actor, &run(p.run)?)?),
        Call::RolesList(_) => value(b.roles_list()?),
        Call::RolesShow(p) => value(b.role_show(&p.slug)?),
        Call::BoardWhoami(_) => Ok(
            json!({"actor":actor,"worker":ctx.worker,"database":b.database_file(),"default_run":ctx.default_run}),
        ),
        Call::BoardChannels(p) => {
            let (cursor, channels, next) =
                b.channels_window("", None, p.prefix.as_deref(), p.limit.map_or(50, |x| x.0))?;
            Ok(json!({"cursor":cursor,"channels":channels,"next":next}))
        }
        Call::BoardFileUpload(p) => {
            let data = STANDARD
                .decode(p.data_base64.0)
                .context("invalid base64 attachment")?;
            ensure!(
                data.len() <= board_local::FILE_CHUNK_BYTES,
                "MCP uploads are limited to 64 KiB"
            );
            value(b.upload(actor, &p.channel, &p.name, &p.content_type, &data)?)
        }
        Call::BoardFileRead(p) => {
            let offset = p.offset.map_or(0, |x| x.0);
            let length = p.length.map_or(board_local::FILE_CHUNK_BYTES, |x| x.0);
            let (file, data) = b.download_range_as(actor, p.id.0, offset, length)?;
            let end = offset + data.len() as i64;
            Ok(
                json!({"attachment":file,"offset":offset,"data_base64":STANDARD.encode(data),"next_offset":(end<file.size).then_some(end)}),
            )
        }
        Call::BoardRead(p) => p.execute(b, ctx),
        Call::SwarmRead(p) => value(b.swarm_read_filtered(
            actor,
            &run(p.run)?,
            p.after.map_or(0, |x| x.0),
            p.limit.map_or(50, |x| x.0),
            true,
            &board_local::swarm::SwarmFilter {
                kind: p.kind.map(|x| x.as_str().into()),
                worker: p.worker,
            },
        )?),
        Call::TasksList(p) => {
            let f = work::TaskFilter {
                channel: p.channel,
                state: p.state,
                assignee: p.assignee,
                ..Default::default()
            };
            let size = p.limit.map_or(50, |x| x.0);
            let (cursor, mut tasks) = work::filtered_tasks_page_as(
                b,
                Some(actor),
                &f,
                p.after.map_or(0, |x| x.0),
                size + 1,
            )?;
            let more = tasks.len() > size;
            tasks.truncate(size);
            Ok(
                json!({"cursor":cursor,"tasks":tasks,"next_after":tasks.last().filter(|_|more).map(|t|t.id)}),
            )
        }
        Call::TasksPlan(p) => {
            let key = p.key.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let (tasks, _) = work::create_plan_for_run(
                b,
                actor,
                &p.channel,
                &p.tasks.0,
                Some(&key),
                p.run.map(read::Run::reference).as_deref(),
            )?;
            value(PlanReply { key, tasks })
        }
        Call::TasksCreate(p) => {
            let token = uuid::Uuid::new_v4().to_string();
            let req = p.claim.unwrap_or(false).then_some(work::ClaimRequest {
                lease_seconds: 300,
                token: &token,
            });
            let (t, _) = work::create_task_in_assignment(
                b,
                actor,
                work::TaskTarget::New {
                    channel: &p.channel,
                    text: &p.text,
                    options: &PostOptions::default(),
                },
                &ids(p.needs),
                p.assignee.as_deref(),
                req,
                ctx.worker.as_ref(),
            )?;
            remember(ctx, t)
        }
        Call::TasksClaim(p) => {
            let token = uuid::Uuid::new_v4().to_string();
            let (t, _) = work::claim_in_assignment(
                b,
                actor,
                p.id.0,
                work::ClaimRequest {
                    lease_seconds: p.lease_seconds.map_or(300, |x| x.0),
                    token: &token,
                },
                ctx.worker.as_ref(),
            )?;
            remember(ctx, t)
        }
        Call::TasksComplete(p) => change(
            b,
            ctx,
            p.id.0,
            work::TaskAction::Complete(p.result.as_deref()),
        ),
        Call::TasksFail(p) => change(b, ctx, p.id.0, work::TaskAction::Fail(&p.reason)),
        Call::TasksBlock(p) => change(b, ctx, p.id.0, work::TaskAction::Block(&p.reason)),
        Call::TasksRenew(p) => change(
            b,
            ctx,
            p.id.0,
            work::TaskAction::Renew(p.lease_seconds.map_or(300, |x| x.0)),
        ),
        Call::TasksRelease(p) => change(b, ctx, p.id.0, work::TaskAction::Release),
        Call::SwarmWait(_) => unreachable!("router parks inbox waits"),
    }
}
