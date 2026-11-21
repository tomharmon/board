mod cache;
mod runner;
use anyhow::{Result, anyhow, ensure};
use board_local::{Board, Filter, PostOptions, automation::*};
use cache::Cache;
use chrono::{DateTime, Utc};
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
    /// Send a message or reply later. Use "schedule cancel ID" to stop it.
    Schedule(ScheduleArgs),
    /// List pending schedules.
    Schedules {
        #[arg(long)]
        all: bool,
        /// Include replies scheduled anywhere in this message's thread
        thread: Option<i64>,
        #[arg(long)]
        by: Option<String>,
    },
    /// Durable named event subscriptions.
    Watchers {
        #[command(subcommand)]
        action: WatchCmd,
    },
    /// Deliver due schedules and expire leases while running.
    Worker {
        #[arg(long)]
        once: bool,
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
    /// Exact channel, or a subtree ending in /.
    channel: Option<String>,
    #[arg(long)]
    state: Option<String>,
    /// List the tasks depending on this prerequisite
    #[arg(long)]
    needs: Option<i64>,
    #[arg(long = "label", value_name = "KEY[=VALUE]")]
    labels: Vec<String>,
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
    #[arg(long, env = "BOARD_CLAIM_TOKEN", hide_env_values = true)]
    token: Option<String>,
}

#[derive(Debug, Args)]
struct ClaimArgs {
    #[arg(required_unless_present = "ready", conflicts_with = "ready")]
    id: Option<i64>,
    #[arg(long)]
    ready: Option<String>,
    #[arg(long = "label", requires = "ready")]
    labels: Vec<String>,
    #[arg(long, default_value = "15m")]
    lease: String,
    #[arg(long, requires = "ready")]
    wait: Option<String>,
    #[arg(long = "from", requires = "ready")]
    owners: Vec<String>,
    /// Caller-minted secret for retries after a lost response
    #[arg(long, env = "BOARD_REQUEST_TOKEN", hide_env_values = true)]
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
        #[arg(long = "label")]
        labels: Vec<String>,
        #[arg(long = "attach", value_name = "FILE", conflicts_with = "file")]
        attachments: Vec<PathBuf>,
        #[arg(long = "attachment-id", conflicts_with = "file")]
        attachment_ids: Vec<String>,
        /// JSON plan; larger than 100 tasks uses resumable chunks. Use - for stdin.
        #[arg(long, conflicts_with_all=["text","needs","assignee","labels"])]
        file: Option<String>,
        #[arg(long)]
        idempotency_key: Option<String>,
        /// Claim the newly created task atomically
        #[arg(long, conflicts_with = "file")]
        claim: bool,
        #[arg(long, requires = "claim")]
        lease: Option<String>,
        #[arg(
            long,
            requires = "claim",
            env = "BOARD_REQUEST_TOKEN",
            hide_env_values = true
        )]
        token: Option<String>,
    },
    /// List open tasks; --state all includes terminal tasks.
    List(TaskList),
    /// Claim by ID or atomically take the oldest matching ready task.
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
        #[arg(long, env = "BOARD_REQUEST_TOKEN", hide_env_values = true)]
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
        #[arg(long, env = "BOARD_CLAIM_TOKEN", hide_env_values = true)]
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
        #[arg(required_unless_present = "pool", conflicts_with = "pool")]
        profile: Option<String>,
        #[arg(long)]
        pool: bool,
    },
    Needs {
        id: i64,
        #[arg(required_unless_present="none",num_args=1..,conflicts_with="none")]
        ids: Vec<i64>,
        #[arg(long)]
        none: bool,
    },
}

#[derive(Debug, Args)]
pub struct ScheduleArgs {
    /// Channel, message ID, or cancel.
    target: String,
    #[arg(conflicts_with = "file")]
    text: Option<String>,
    #[arg(long)]
    file: Option<PathBuf>,
    #[arg(long = "attach", value_name = "FILE")]
    attachments: Vec<PathBuf>,
    #[arg(long = "attachment-id")]
    attachment_ids: Vec<String>,
    #[arg(long = "in", conflicts_with = "at")]
    delay: Option<String>,
    #[arg(long)]
    at: Option<String>,
    #[arg(long)]
    every: Option<String>,
    #[arg(long,requires="every",conflicts_with_all=["until","span"])]
    times: Option<i64>,
    #[arg(long, requires = "every", conflicts_with = "span")]
    until: Option<String>,
    #[arg(long = "for", requires = "every")]
    span: Option<String>,
    #[arg(long = "label")]
    labels: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum WatchCmd {
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
        channel: Option<String>,
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
        #[arg(long = "label")]
        labels: Vec<String>,
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
fn date(s: &str) -> Result<i64> {
    Ok(DateTime::parse_from_rfc3339(s)?.timestamp())
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
        channel: q.channel.clone(),
        state: q.state.clone(),
        needs: q.needs,
        labels: Default::default(),
        label_terms: board_local::LabelTerm::parse(&q.labels)?,
        by: q.by.clone(),
        owner: q.owner.clone(),
        assignee: q.assignee.as_deref().map(|p| assignee(actor, p)),
    };
    let (_, mut ts) = filtered_tasks_page(board, &filter, q.after, q.limit + 1)?;
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
fn take(board: &Board, actor: &str, args: &ClaimArgs, cache: &Cache) -> Result<Task> {
    let lease = secs(&args.lease)?;
    let labels = crate::pairs(&args.labels, false)?;
    let wait = args
        .wait
        .as_deref()
        .map(humantime::parse_duration)
        .transpose()?
        .unwrap_or_default();
    let proof = cache.prepare(args.token.as_deref())?;
    let start = Instant::now();
    loop {
        let target = args.id.map_or_else(
            || ClaimTarget::Ready(args.ready.as_deref().unwrap_or("")),
            ClaimTarget::Id,
        );
        match claim_with_token(
            board,
            actor,
            target,
            ClaimRequest {
                lease_seconds: lease,
                token: &proof,
            },
            &labels,
            &args.owners,
        ) {
            Ok(t) => {
                cache.remember(&t)?;
                return Ok(t);
            }
            Err(e) if e.is::<NothingReady>() && start.elapsed() < wait => {
                sleep(Duration::from_millis(200).min(wait.saturating_sub(start.elapsed())))
            }
            Err(e) => return Err(e),
        }
    }
}
fn task_command(cmd: TaskArgs, board: &Board, actor: &str, json: bool, path: &Path) -> Result<()> {
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
            labels: ls,
            attachments,
            attachment_ids,
            file,
            idempotency_key: key,
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
                let plan = create_plan(board, actor, target, &items, key.as_deref())?;
                return print(&plan, json, || {
                    plan.iter().map(summary).collect::<Vec<_>>().join("\n")
                });
            }
            let opts = PostOptions {
                labels: crate::pairs(ls, false)?,
                files: crate::attachment_files(attachments)?,
                attachment_ids: attachment_ids
                    .iter()
                    .map(|key| board.file_id(actor, key))
                    .collect::<Result<Vec<_>>>()?,
                key: key.clone(),
                ..Default::default()
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
                        ls.is_empty() && key.is_none(),
                        "labels and idempotency apply to new threads only"
                    );
                    TaskTarget::Existing(target.parse()?)
                }
            };
            let cache = claim.then(cached).transpose()?;
            let proof = cache
                .as_ref()
                .map(|c| c.prepare(token.as_deref()))
                .transpose()?;
            let lease = lease.as_deref().map(secs).transpose()?.unwrap_or(900);
            let (mut t, _) = create_task_and_claim(
                board,
                actor,
                target,
                needs,
                who.as_deref().map(|p| assignee(actor, p)).as_deref(),
                proof.as_deref().map(|token| ClaimRequest {
                    lease_seconds: lease,
                    token,
                }),
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
        Claim(args) => take(board, actor, args, &cached()?)?,
        Run {
            claim,
            result,
            command,
        } => {
            let cache = cached()?;
            let t = take(board, actor, claim, &cache)?;
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
pub(crate) fn sched_line(s: &Schedule) -> String {
    format!(
        "s{}  {:8}  {}  {}  {}",
        s.id,
        s.state,
        s.channel,
        DateTime::<Utc>::from_timestamp(s.due, 0)
            .map(|t| t.to_rfc3339())
            .unwrap_or_default(),
        s.text
    )
}

impl Commands {
    pub fn run(self, board: &mut Board, actor: &str, json: bool, path: &Path) -> Result<()> {
        match self {
            Self::Tasks(cmd) => task_command(*cmd, board, actor, json, path),
            Self::Schedule(a) => {
                let s = if a.target == "cancel" {
                    cancel_schedule(
                        board,
                        a.text
                            .as_deref()
                            .ok_or_else(|| anyhow!("cancel requires a schedule id"))?
                            .trim_start_matches('s')
                            .parse()?,
                        actor,
                    )?
                } else {
                    let now = Utc::now().timestamp();
                    let every = a.every.as_deref().map(secs).transpose()?;
                    let due = if let Some(at) = a.at {
                        date(&at)?
                    } else {
                        now.checked_add(
                            a.delay
                                .as_deref()
                                .map(secs)
                                .transpose()?
                                .or(every)
                                .ok_or_else(|| anyhow!("provide --in, --at or --every"))?,
                        )
                        .ok_or_else(|| anyhow!("time overflow"))?
                    };
                    let until = if let Some(span) = a.span {
                        Some(
                            due.checked_add(secs(&span)?)
                                .ok_or_else(|| anyhow!("time overflow"))?,
                        )
                    } else {
                        a.until.as_deref().map(date).transpose()?
                    };
                    let dest = a
                        .target
                        .parse::<i64>()
                        .map_or(Destination::Channel(&a.target), Destination::Reply);
                    let text = match a.file.as_deref() {
                        Some(p) => crate::load_text(p)?,
                        None => a.text.unwrap_or_default(),
                    };
                    schedule_post(
                        board,
                        actor,
                        dest,
                        &text,
                        Timing {
                            due,
                            every,
                            times: a.times,
                            until,
                        },
                        &PostOptions {
                            labels: crate::pairs(&a.labels, false)?,
                            files: crate::attachment_files(&a.attachments)?,
                            attachment_ids: a
                                .attachment_ids
                                .iter()
                                .map(|key| board.file_id(actor, key))
                                .collect::<Result<Vec<_>>>()?,
                            ..Default::default()
                        },
                    )?
                };
                print(&s, json, || sched_line(&s))
            }
            Self::Schedules { all, thread, by } => {
                let s = schedule_query(board, all, thread, by.as_deref(), 0, i64::MAX, false)?.1;
                print(&s, json, || {
                    s.iter().map(sched_line).collect::<Vec<_>>().join("\n")
                })
            }
            Self::Watchers { action } => match action {
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
                    channel,
                    kind,
                    by,
                    re,
                    text,
                    mentions,
                    labels: ls,
                } => {
                    let w = add_watcher_as(
                        board,
                        actor,
                        &name,
                        Filter {
                            channel,
                            by,
                            re,
                            text,
                            mentions,
                            label_terms: board_local::LabelTerm::parse(&ls)?,
                            ..Default::default()
                        },
                        kind.as_deref(),
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
                    skip,
                    error,
                } => {
                    let (outcome, action) = match (skip, error.as_deref()) {
                        (true, _) => (DeliveryOutcome::Skip, "skipped"),
                        (_, Some(why)) => (DeliveryOutcome::Error(why), "recorded failure for"),
                        _ => (DeliveryOutcome::Ack, "acknowledged"),
                    };
                    settle_delivery(board, &name, cursor, receipt.as_deref(), outcome)?;
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
            Self::Worker { once } => loop {
                let expired = expire_leases(board)?;
                let sent = dispatch_due(board)?;
                if once || expired + sent > 0 {
                    print(
                        &serde_json::json!({"sent":sent,"expired":expired}),
                        json,
                        || format!("sent {sent}, expired {expired}"),
                    )?;
                }
                if once {
                    break Ok(());
                }
                sleep(Duration::from_secs(1));
            },
        }
    }
}
