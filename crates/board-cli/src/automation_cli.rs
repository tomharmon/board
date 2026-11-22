mod cache;
mod runner;
use anyhow::{Context, Result, ensure};
use board_local::{Board, Filter, PostOptions, automation::*};
use cache::Cache;
use clap::{Args, Subcommand};
pub(crate) use runner::RunExit;
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    thread::sleep,
    time::{Duration, Instant},
};

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Create, route and lease work on threads.
    Tasks(Box<TaskArgs>),
    /// Durable named event subscriptions.
    Watchers {
        #[command(subcommand)]
        action: Box<WatchCmd>,
    },
}

#[derive(Debug, Args)]
pub struct TaskArgs {
    #[command(subcommand)]
    action: Option<TaskCmd>,
    #[command(flatten)]
    query: TaskList,
}

#[derive(Debug, Args)]
struct TaskList {
    /// Return the most recently active tasks, in chronological order.
    #[arg(long, conflicts_with = "after")]
    latest: bool,
    /// Exact channel, or a subtree ending in /.
    channel: Option<String>,
    #[arg(long)]
    state: Option<String>,
    /// List the tasks depending on this prerequisite
    #[arg(long)]
    needs: Option<i64>,
    #[arg(long)]
    by: Option<String>,
    #[arg(long)]
    owner: Option<String>,
    #[arg(long = "for")]
    assignee: Option<String>,
    #[arg(long, default_value_t = 0)]
    after: i64,
    #[arg(long, default_value_t = 50)]
    limit: usize,
}

#[derive(Debug, Args)]
struct Held {
    id: i64,
    /// Generation returned by claim. Rejects writes from expired runs.
    #[arg(long, env = "BOARD_CLAIM_EPOCH")]
    epoch: Option<i64>,
    /// Private receipt returned only to the successful claimant
    #[arg(
        long,
        env = "BOARD_CLAIM_TOKEN",
        hide_env_values = true,
        allow_hyphen_values = true
    )]
    token: Option<String>,
}

#[derive(Debug, Args)]
struct ClaimArgs {
    id: i64,
    #[arg(long, default_value = "15m")]
    lease: String,
    /// Caller-minted secret for retries after a lost response
    #[arg(
        long,
        env = "BOARD_REQUEST_TOKEN",
        hide_env_values = true,
        allow_hyphen_values = true
    )]
    token: Option<String>,
}

#[derive(Debug, Subcommand)]
enum TaskCmd {
    /// Start a task thread, or promote an existing root by ID.
    Create {
        target: String,
        text: Option<String>,
        #[arg(long)]
        needs: Vec<i64>,
        #[arg(long = "for")]
        assignee: Option<String>,
        #[arg(long = "attach", value_name = "FILE", conflicts_with = "file")]
        attachments: Vec<PathBuf>,
        #[arg(long = "attachment-id", conflicts_with = "file")]
        attachment_ids: Vec<String>,
        /// Atomic JSON plan (1-100 tasks). Use - for stdin.
        #[arg(long, conflicts_with_all=["text","needs","assignee"])]
        file: Option<String>,
        /// Associate the plan's task threads with this swarm (does not change access or claim them).
        #[arg(long, requires = "file")]
        run: Option<String>,
        #[arg(long)]
        idempotency_key: Option<String>,
        #[arg(long, conflicts_with = "file")]
        no_sync: bool,
        /// Claim the newly created task atomically
        #[arg(long, conflicts_with = "file")]
        claim: bool,
        #[arg(long, requires = "claim")]
        lease: Option<String>,
        #[arg(
            long,
            requires = "claim",
            env = "BOARD_REQUEST_TOKEN",
            hide_env_values = true,
            allow_hyphen_values = true
        )]
        token: Option<String>,
    },
    /// List open tasks; --state all includes terminal tasks.
    List(TaskList),
    /// Lease the specified task.
    Claim(ClaimArgs),
    /// Run an explicitly supplied program under a renewable task lease
    Run {
        #[command(flatten)]
        claim: ClaimArgs,
        #[arg(long)]
        result: Option<String>,
        #[arg(last=true,num_args=1..,required=true)]
        command: Vec<std::ffi::OsString>,
    },
    /// Recover a live claim after losing its response
    Recover {
        #[arg(required_unless_present = "token")]
        id: Option<i64>,
        #[arg(
            long,
            env = "BOARD_REQUEST_TOKEN",
            hide_env_values = true,
            allow_hyphen_values = true
        )]
        token: Option<String>,
    },
    Renew {
        #[command(flatten)]
        held: Held,
        #[arg(long, default_value = "15m")]
        lease: String,
    },
    Release(Held),
    Complete {
        #[command(flatten)]
        held: Held,
        result: Option<String>,
    },
    Fail {
        #[command(flatten)]
        held: Held,
        reason: String,
    },
    Block {
        id: i64,
        reason: String,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, env = "BOARD_CLAIM_EPOCH")]
        epoch: Option<i64>,
        #[arg(
            long,
            env = "BOARD_CLAIM_TOKEN",
            hide_env_values = true,
            allow_hyphen_values = true
        )]
        token: Option<String>,
    },
    Unblock {
        id: i64,
    },
    Reopen {
        id: i64,
    },
    Cancel {
        id: i64,
        reason: String,
    },
    Assign {
        id: i64,
        #[arg(required_unless_present = "none", conflicts_with = "none")]
        profile: Option<String>,
        #[arg(long)]
        none: bool,
    },
    Needs {
        id: i64,
        #[arg(required_unless_present="none",num_args=1..,conflicts_with="none")]
        ids: Vec<i64>,
        #[arg(long)]
        none: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum WatchCmd {
    /// Lease independent thread heads from all registrations visible to this actor.
    Matches {
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long, default_value = "5m")]
        lease: String,
    },
    /// Atomically replace this actor's subscriptions from a JSON array; - reads stdin.
    Replace {
        file: PathBuf,
    },
    /// Run an explicit local handler. It receives the event JSON on stdin.
    Run {
        name: String,
        #[arg(long)]
        once: bool,
        #[arg(long, default_value = "30s")]
        lease: String,
        #[arg(last=true,num_args=1..,required=true)]
        command: Vec<std::ffi::OsString>,
    },
    /// Subscribe to future matching events, from the current head.
    Add {
        name: String,
        #[arg(long)]
        all: bool,
        channel: Option<String>,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        by: Option<String>,
        #[arg(long)]
        re: Option<i64>,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        mentions: Option<String>,
    },
    List,
    /// Deliver the next match. It remains pending until acknowledged.
    Next {
        name: String,
        #[arg(long)]
        wait: Option<String>,
        /// Lease the delivery exclusively; include its receipt when acknowledging
        #[arg(long)]
        lease: Option<String>,
    },
    /// Acknowledge exactly the pending delivery, not a future cursor.
    Ack {
        name: String,
        cursor: i64,
        #[arg(long, env = "BOARD_DELIVERY_RECEIPT", hide_env_values = true)]
        receipt: Option<String>,
        /// Require a still-pending lease; a proven repeat reports no_match instead of success
        #[arg(long, requires = "receipt")]
        if_pending: bool,
        /// Intentionally skip this event, allowing its thread to advance
        #[arg(long, conflicts_with = "error")]
        skip: bool,
        /// Record a failed delivery; retry with backoff before giving up
        #[arg(long)]
        error: Option<String>,
    },
    /// Inspect events whose delivery exhausted its retries
    Failures {
        name: String,
        #[arg(long, default_value_t = 0)]
        after: i64,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    Remove {
        name: String,
    },
}

fn secs(s: &str) -> Result<i64> {
    let d = humantime::parse_duration(s)?;
    ensure!(d.subsec_nanos() == 0, "duration must be whole seconds");
    Ok(i64::try_from(d.as_secs())?)
}
fn poll_next<T>(wait: Duration, mut pull: impl FnMut() -> Result<Option<T>>) -> Result<Option<T>> {
    let start = Instant::now();
    loop {
        let item = pull()?;
        if item.is_some() || start.elapsed() >= wait {
            return Ok(item);
        }
        sleep(Duration::from_millis(200).min(wait.saturating_sub(start.elapsed())));
    }
}
fn print<T: Serialize>(data: &T, json: bool, line: impl FnOnce() -> String) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(data)?);
    } else {
        println!("{}", crate::safe(&line()));
    }
    Ok(())
}
fn person(actor: &str) -> &str {
    actor.split('/').next().unwrap_or(actor)
}
fn assignee(actor: &str, profile: &str) -> String {
    if profile.contains('/') || profile == person(actor) {
        profile.to_owned()
    } else {
        format!("{}/{}", person(actor), profile)
    }
}
pub(crate) fn summary(t: &Task) -> String {
    let mut line = format!(
        "#{}  {:9}  {}",
        t.id,
        t.state,
        t.text.lines().next().unwrap_or("")
    );
    if let Some(h) = &t.holder {
        line.push_str(&format!(
            "  [{h}, epoch {}, expires {}]",
            t.epoch,
            t.lease_until.unwrap_or_default()
        ));
    }
    if let Some(result) = &t.result {
        line.push_str(&format!("  {result}"));
    }
    line
}
fn list(board: &Board, actor: &str, q: &TaskList, json: bool) -> Result<()> {
    ensure!((1..=500).contains(&q.limit), "limit must be 1..500");
    let filter = TaskFilter {
        latest: q.latest,
        channel: q.channel.clone(),
        state: q.state.clone(),
        needs: q.needs,
        by: q.by.clone(),
        owner: q.owner.clone(),
        assignee: q.assignee.as_deref().map(|p| assignee(actor, p)),
    };
    let (_, mut ts) = filtered_tasks_page_as(
        board,
        Some(actor),
        &filter,
        q.after,
        q.limit + usize::from(!q.latest),
    )?;
    let more = ts.len() > q.limit;
    ts.truncate(q.limit);
    print(&ts, json, || {
        let mut lines = ts.iter().map(summary).collect::<Vec<_>>();
        if more {
            lines.push(format!("(more: --after {})", ts.last().unwrap().id));
        }
        lines.join("\n")
    })
}
pub(crate) fn output_key(
    board: &Board,
    actor: &str,
    given: Option<&str>,
) -> Result<Option<String>> {
    if let Some(token) = std::env::var_os("BOARD_CALLBACK_TOKEN") {
        return Ok(Some(callback_write_key(
            board,
            actor,
            token.to_str().context("invalid callback token")?,
            given,
        )?));
    }
    Ok(given.map(str::to_owned))
}
fn take(
    board: &Board,
    actor: &str,
    args: &ClaimArgs,
    cache: &Cache,
    binding: Option<&board_local::swarm::WorkerBinding>,
) -> Result<Task> {
    let lease_seconds = secs(&args.lease)?;
    let proof = cache.prepare(args.token.as_deref())?;
    let (t, _) = claim_in_assignment(
        board,
        actor,
        args.id,
        ClaimRequest {
            lease_seconds,
            token: &proof,
        },
        binding,
    )?;
    cache.remember(&t)?;
    Ok(t)
}
fn task_command(
    cmd: TaskArgs,
    board: &Board,
    actor: &str,
    json: bool,
    path: &Path,
    binding: Option<&board_local::swarm::WorkerBinding>,
) -> Result<()> {
    use TaskCmd::*;
    let Some(action) = cmd.action else {
        return list(board, actor, &cmd.query, json);
    };
    let change = |id, epoch, token, op| change_task(board, id, actor, epoch, token, op);
    let cached = || Cache::open(board, path, actor);
    let held = |h: &Held, op: TaskAction<'_>| -> Result<Task> {
        let cache = cached()?;
        let (epoch, token) = match (h.epoch, h.token.as_deref()) {
            (Some(e), Some(t)) => (e, t.to_owned()),
            (Some(_), None) => anyhow::bail!("provide both epoch and token, or neither"),
            (None, Some(t)) => {
                let claim = recover_claim(board, actor, t)?;
                ensure!(claim.id == h.id, "token is for a different task");
                (claim.epoch, t.to_owned())
            }
            (None, None) => {
                let claim = cache.held(board, actor, h.id)?;
                (claim.epoch, claim.claim_token.unwrap())
            }
        };
        let renew = matches!(op, TaskAction::Renew(_));
        let t = change_task(board, h.id, actor, Some(epoch), Some(&token), op)?;
        if !renew {
            cache.forget(&token)?;
        }
        Ok(t)
    };
    let t = match &action {
        List(query) => return list(board, actor, query, json),
        Create {
            target,
            text,
            needs,
            assignee: who,
            attachments,
            attachment_ids,
            file,
            run,
            idempotency_key: key,
            no_sync,
            claim,
            lease,
            token,
        } => {
            if let Some(file) = file {
                let items: Vec<PlanItem> = if file == "-" {
                    serde_json::from_reader(std::io::stdin().lock())?
                } else {
                    serde_json::from_reader(std::fs::File::open(file)?)?
                };
                let key = output_key(board, actor, key.as_deref())?;
                let (plan, _) = create_plan_for_run(
                    board,
                    actor,
                    target,
                    &items,
                    key.as_deref(),
                    run.as_deref(),
                )?;
                return print(&plan, json, || {
                    plan.iter().map(summary).collect::<Vec<_>>().join("\n")
                });
            }
            let opts = PostOptions {
                key: if text.is_some() || !attachments.is_empty() || !attachment_ids.is_empty() {
                    output_key(board, actor, key.as_deref())?
                } else {
                    None
                },
                no_sync: *no_sync,
                ..crate::attachments::placements(board, actor, target, attachments, attachment_ids)?
            };
            let input = match text.as_deref() {
                Some("-") => Some(std::io::read_to_string(std::io::stdin().lock())?),
                _ => None,
            };
            let target = match input.as_deref().or(text.as_deref()) {
                text if text.is_some() || !attachments.is_empty() || !attachment_ids.is_empty() => {
                    TaskTarget::New {
                        channel: target,
                        text: text.unwrap_or_default(),
                        options: &opts,
                    }
                }
                _ => {
                    ensure!(
                        key.is_none() && !*no_sync,
                        "idempotency and no-sync apply to new threads only"
                    );
                    TaskTarget::Existing(crate::parse_id(target).map_err(anyhow::Error::msg)?)
                }
            };
            let cache = claim.then(cached).transpose()?;
            let proof = cache
                .as_ref()
                .map(|c| c.prepare(token.as_deref()))
                .transpose()?;
            let lease = lease.as_deref().map(secs).transpose()?.unwrap_or(900);
            let (mut t, _) = create_task_in_assignment(
                board,
                actor,
                target,
                needs,
                who.as_deref().map(|p| assignee(actor, p)).as_deref(),
                proof.as_deref().map(|token| ClaimRequest {
                    lease_seconds: lease,
                    token,
                }),
                binding,
            )?;
            if let Some(cache) = cache {
                if t.claim_token.is_none()
                    && t.state == "claimed"
                    && let Ok(recovered) = cache.held(board, actor, t.id)
                {
                    t = recovered;
                }
                cache.remember(&t)?;
            }
            t
        }
        Claim(args) => take(board, actor, args, &cached()?, binding)?,
        Run {
            claim,
            result,
            command,
        } => {
            let cache = cached()?;
            let t = take(board, actor, claim, &cache, binding)?;
            return runner::run(
                board,
                actor,
                &t,
                secs(&claim.lease)?,
                command,
                result.as_deref(),
                json,
                &cache,
                path,
            );
        }
        Recover { id, token } => match token {
            Some(t) => {
                let task = recover_claim(board, actor, t)?;
                ensure!(
                    id.is_none_or(|id| id == task.id),
                    "token is for a different task"
                );
                let c = cached()?;
                c.prepare(Some(t))?;
                c.remember(&task)?;
                task
            }
            None => cached()?.held(board, actor, id.unwrap())?,
        },
        Renew { held: h, lease } => held(h, TaskAction::Renew(secs(lease)?))?,
        Release(h) => held(h, TaskAction::Release)?,
        Complete { held: h, result } => held(h, TaskAction::Complete(result.as_deref()))?,
        Fail { held: h, reason } => held(h, TaskAction::Fail(reason))?,
        Block {
            id,
            reason,
            kind,
            epoch,
            token,
        } => {
            let action = kind.as_deref().map_or(TaskAction::Block(reason), |kind| {
                TaskAction::BlockWithKind(reason, kind)
            });
            if epoch.is_some() || token.is_some() || get_task(board, *id)?.state == "claimed" {
                held(
                    &Held {
                        id: *id,
                        epoch: *epoch,
                        token: token.clone(),
                    },
                    action,
                )?
            } else {
                change(*id, None, None, action)?
            }
        }
        Unblock { id } => change(*id, None, None, TaskAction::Unblock)?,
        Reopen { id } => change(*id, None, None, TaskAction::Reopen)?,
        Cancel { id, reason } => change(*id, None, None, TaskAction::Cancel(reason))?,
        Assign { id, profile, .. } => change_task(
            board,
            *id,
            actor,
            None,
            None,
            TaskAction::Assign(profile.as_deref().map(|p| assignee(actor, p)).as_deref()),
        )?,
        Needs { id, ids, .. } => change(*id, None, None, TaskAction::Needs(ids))?,
    };
    print(&t, json, || {
        if matches!(action, Create { .. }) {
            t.id.to_string()
        } else {
            summary(&t)
        }
    })
}

impl Commands {
    pub fn run(
        self,
        board: &mut Board,
        actor: &str,
        json: bool,
        path: &Path,
        binding: Option<&board_local::swarm::WorkerBinding>,
    ) -> Result<()> {
        match self {
            Self::Tasks(cmd) => task_command(*cmd, board, actor, json, path, binding),
            Self::Watchers { action } => match *action {
                WatchCmd::Matches { limit, lease } => {
                    let found = lease_matches(board, limit, secs(&lease)?)?;
                    print(&found, json, || {
                        found
                            .iter()
                            .map(|d| {
                                format!("{} #{} receipt {}", d.watcher, d.event.cursor, d.receipt)
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                }
                WatchCmd::Replace { file } => {
                    let specs: Vec<WatchSpec> = serde_json::from_str(&crate::load_text(&file)?)?;
                    let ws = replace_watchers(board, actor, &specs)?;
                    print(&ws, json, || {
                        ws.iter()
                            .map(|w| w.name.as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                }
                WatchCmd::Run {
                    name,
                    once,
                    lease,
                    command,
                } => runner::watch(
                    board,
                    actor,
                    &name,
                    secs(&lease)?,
                    once,
                    &command,
                    json,
                    path,
                ),
                WatchCmd::Add {
                    name,
                    all,
                    channel,
                    from,
                    kind,
                    by,
                    re,
                    text,
                    mentions,
                } => {
                    let w = add_watcher_spec(
                        board,
                        actor,
                        WatchSpec {
                            name,
                            all,
                            kind,
                            filter: Filter {
                                channel,
                                from,
                                by,
                                re,
                                text,
                                mentions,
                                ..Default::default()
                            },
                        },
                    )?;
                    print(&w, json, || format!("{}  after {}", w.name, w.cursor))
                }
                WatchCmd::List => {
                    let w = watchers(board)?;
                    print(&w, json, || {
                        w.iter()
                            .map(|w| format!("{}  after {}", w.name, w.cursor))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                }
                WatchCmd::Next { name, wait, lease } => {
                    let wait = wait
                        .as_deref()
                        .map(humantime::parse_duration)
                        .transpose()?
                        .unwrap_or_default();
                    if let Some(duration) = lease {
                        let lease = secs(&duration)?;
                        let delivery = poll_next(wait, || lease_event(board, &name, lease))?;
                        return print(&delivery, json, || {
                            delivery
                                .as_ref()
                                .map(|d| {
                                    format!(
                                        "{}  {}  receipt: {}",
                                        d.event.cursor, d.event.kind, d.receipt
                                    )
                                })
                                .unwrap_or_else(|| "nothing pending".into())
                        });
                    }
                    let ev = poll_next(wait, || next_event(board, &name))?;
                    print(&ev, json, || {
                        ev.as_ref()
                            .map(|ev| format!("{}  {}", ev.cursor, ev.kind))
                            .unwrap_or_else(|| "nothing pending".into())
                    })
                }
                WatchCmd::Ack {
                    name,
                    cursor,
                    receipt,
                    if_pending,
                    skip,
                    error,
                } => {
                    let (outcome, action) = match (skip, error.as_deref()) {
                        (true, _) => (DeliveryOutcome::Skip, "skipped"),
                        (_, Some(why)) => (DeliveryOutcome::Error(why), "recorded failure for"),
                        _ => (DeliveryOutcome::Ack, "acknowledged"),
                    };
                    settle_delivery_if(
                        board,
                        &name,
                        cursor,
                        receipt.as_deref(),
                        outcome,
                        if_pending,
                    )?;
                    print(&cursor, json, || format!("{action} {cursor}"))
                }
                WatchCmd::Failures { name, after, limit } => {
                    ensure!((1..=500).contains(&limit), "limit must be 1 to 500");
                    let (snapshot, failures) = watcher_failures(board, &name, after, limit)?;
                    let cursor = failures.last().map_or(after, |row| row.cursor);
                    print(
                        &serde_json::json!({"failures":failures,"cursor":cursor,"snapshot":snapshot}),
                        json,
                        || {
                            failures
                                .iter()
                                .map(|f| {
                                    format!(
                                        "{}: {} ({} attempts)",
                                        f.cursor,
                                        crate::safe(&f.error),
                                        f.attempts
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                        },
                    )
                }
                WatchCmd::Remove { name } => {
                    remove_watcher_as(board, actor, &name)?;
                    print(&name, json, || format!("removed {name}"))
                }
            },
        }
    }
}
