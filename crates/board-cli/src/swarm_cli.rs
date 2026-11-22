use crate::{attachments, cli::Body, roles_cli::output};
use anyhow::{Context, Result, ensure};
use board_local::{
    Board,
    swarm::{RunPolicy, SwarmAddWorker, SwarmStart},
};
use clap::Subcommand;
use serde_json::{Value, json};
use std::{
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicI32, Ordering},
    },
    time::Duration,
};

#[derive(Subcommand)]
pub enum Swarm {
    /// Create a local run. No agent process is started.
    Start {
        goal: String,
        #[arg(long)]
        slug: Option<String>,
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long)]
        max_notifications: Option<i64>,
        #[arg(long, default_value_t = 8)]
        max_active: i64,
        #[arg(long, default_value_t = 0)]
        retry_budget: i64,
    },
    /// Register a worker and render the pinned prompt and environment for your harness.
    Add {
        worker_name: String,
        #[arg(long)]
        run: Option<String>,
        #[arg(long)]
        role: Option<String>,
        #[arg(long)]
        brief: String,
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[arg(long)]
        branch: Option<String>,
    },
    Status {
        run: Option<String>,
    },
    /// Read current exceptions, counts and a bounded page of changes.
    Observe {
        #[arg(long)]
        run: Option<String>,
        #[arg(long)]
        after: Option<i64>,
        #[arg(long, default_value_t = 0)]
        action_after: i64,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Explicitly announce that this worker has started. Does not launch a process.
    CheckIn {
        #[arg(long)]
        run: Option<String>,
        #[arg(long)]
        compact: bool,
    },
    /// Report a launch error observed by the harness, never inferred from silence.
    LaunchFailed {
        worker_name: String,
        reason: String,
        #[arg(long)]
        run: Option<String>,
    },
    /// Show immutable handoffs, revision-bound checks and unresolved decisions.
    Receipt {
        #[arg(long)]
        run: Option<String>,
    },
    /// List operator decisions separately from agent follow-up.
    Attention,
    /// Answer an explicit decision request, including after a run has stopped.
    Decide {
        id: i64,
        summary: String,
        #[arg(long)]
        run: Option<String>,
    },
    List {
        #[arg(long)]
        before: Option<i64>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// A public post. Plain text is an FYI; @name notifies an existing teammate.
    Post {
        #[command(flatten)]
        body: Body,
        #[arg(long)]
        run: Option<String>,
        /// Append a structured diff@1 block from git diff (use --diff=RANGE for a revision range).
        #[arg(
            long,
            num_args = 0..=1,
            default_missing_value = "",
            require_equals = true,
            value_name = "RANGE"
        )]
        diff: Option<String>,
        /// JSON payload for --schema when combining prose/@mentions with a structured block.
        #[arg(
            long,
            requires = "schema",
            conflicts_with = "schema_file",
            value_name = "JSON"
        )]
        schema_data: Option<String>,
        /// File containing JSON (or - for stdin) for --schema alongside prose text.
        #[arg(long, requires = "schema", value_name = "FILE")]
        schema_file: Option<PathBuf>,
        #[arg(long, value_name = "KEY=VALUE")]
        label: Vec<String>,
        /// Retry this write without duplicating it.
        #[arg(long = "idempotency-key", visible_alias = "id")]
        key: Option<String>,
        #[arg(long, value_name = "FILE")]
        attach: Vec<PathBuf>,
    },
    /// A side conversation visible to its participants, the coordinator and the board owner.
    Dm {
        #[arg(long)]
        to: String,
        text: Option<String>,
        #[arg(long, conflicts_with = "text")]
        file: Option<PathBuf>,
        #[arg(long)]
        run: Option<String>,
    },
    Read {
        #[arg(long)]
        run: Option<String>,
        #[arg(long, default_value_t = 0)]
        after: i64,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Sleep until notified, timeout, or stop. Omit --after to use your saved read position.
    Wait {
        #[arg(long)]
        run: Option<String>,
        #[arg(long)]
        after: Option<i64>,
        #[arg(long, default_value = "5m")]
        timeout: String,
    },
    Done {
        summary: Option<String>,
        #[arg(long)]
        run: Option<String>,
    },
    Stop {
        run: Option<String>,
    },
    SetLimit {
        #[arg(long)]
        run: Option<String>,
        limit: i64,
    },
}
impl Swarm {
    pub(crate) fn select_run(
        &mut self,
        binding: Option<&board_local::swarm::WorkerBinding>,
    ) -> Result<()> {
        if binding.is_some()
            && matches!(
                self,
                Self::Start { .. }
                    | Self::Add { .. }
                    | Self::SetLimit { .. }
                    | Self::Stop { .. }
                    | Self::List { .. }
                    | Self::LaunchFailed { .. }
            )
        {
            return Err(board_local::swarm::WorkerBinding::coordinator_only());
        }
        let run = match self {
            Self::Post { run, .. }
            | Self::Dm { run, .. }
            | Self::Read { run, .. }
            | Self::Wait { run, .. }
            | Self::Done { run, .. }
            | Self::Status { run }
            | Self::Observe { run, .. }
            | Self::CheckIn { run, .. }
            | Self::LaunchFailed { run, .. }
            | Self::Receipt { run }
            | Self::Decide { run, .. }
            | Self::Add { run, .. }
            | Self::SetLimit { run, .. }
            | Self::Stop { run } => run,
            Self::Start { .. } | Self::List { .. } | Self::Attention => return Ok(()),
        };
        if let Some(binding) = binding {
            *run = Some(binding.select_run(run.as_deref())?);
        } else if run.is_none() {
            *run = std::env::var("BOARD_SWARM_RUN").ok();
        }
        Ok(())
    }
}
fn reference(run: Option<&str>) -> Result<&str> {
    run.context(
        "select a run with --run (or positional RUN for status/stop), or set BOARD_SWARM_RUN",
    )
}
fn local_path(p: PathBuf) -> Result<String> {
    Ok(p.canonicalize()
        .with_context(|| format!("open {}", p.display()))?
        .to_str()
        .context("expected a UTF-8 path")?
        .into())
}

fn git_diff(range: &str, names: bool, limit: usize) -> Result<Vec<u8>> {
    ensure!(
        !range.starts_with('-') && range.len() <= 512,
        "expected a Git revision or range, not an option"
    );
    let mut cmd = Command::new("git");
    cmd.args([
        "--no-pager",
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
    ]);
    if names {
        cmd.args(["--name-only", "-z"]);
    }
    if !range.is_empty() {
        cmd.arg(range);
    }
    cmd.arg("--");
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .context("run git diff")?;
    let mut data = Vec::new();
    let read = child
        .stdout
        .take()
        .context("missing git stdout")?
        .take((limit + 1) as u64)
        .read_to_end(&mut data);
    if read.is_err() || data.len() > limit {
        let _ = child.kill();
    }
    let status = child.wait()?;
    read?;
    ensure!(
        data.len() <= limit,
        "diff is too large; narrow the revision or use --attach FILE"
    );
    ensure!(status.success(), "git diff failed ({status})");
    Ok(data)
}
fn with_diff(text: &mut String, range: &str, opts: &mut board_local::PostOptions) -> Result<()> {
    let patch = git_diff(range, false, board_local::MAX_ATTACHMENT_BYTES)?;
    let raw_files = git_diff(range, true, 128 * 1024)?;
    let files: Vec<_> = raw_files
        .split(|b| *b == 0)
        .filter(|v| !v.is_empty())
        .take(32)
        .map(|s| {
            String::from_utf8_lossy(s)
                .chars()
                .take(180)
                .collect::<String>()
        })
        .collect();
    let (base, head) = range
        .split_once("...")
        .or_else(|| range.split_once(".."))
        .unwrap_or((range, "working tree"));
    let mut v = json!({"patch":String::from_utf8_lossy(&patch),"files":files,"head":head});
    if !base.is_empty() {
        v["base"] = json!(base);
    }
    if text.encode_utf16().count() + v.to_string().encode_utf16().count() + 32 > 9800 {
        ensure!(
            opts.files.len() + opts.attachment_ids.len() < board_local::MAX_ATTACHMENTS,
            "leave one attachment slot for the large diff"
        );
        v["patch"] = json!("Full patch attached as changes.diff");
        v["attached"] = json!("changes.diff");
        if text.len() + v.to_string().len() > 9800 {
            v["files"] = json!([]);
        }
        opts.files.push(("changes.diff".into(), patch));
    }
    *text = board_local::append_block(text, "diff", 1, &v)?;
    Ok(())
}

struct WaitSignals {
    signal: Arc<AtomicI32>,
    #[cfg(unix)]
    handlers: Vec<signal_hook_registry::SigId>,
}
impl WaitSignals {
    fn install() -> Result<Self> {
        let signals = Self {
            signal: Arc::new(AtomicI32::new(0)),
            #[cfg(unix)]
            handlers: Vec::new(),
        };
        #[cfg(unix)]
        {
            let mut signals = signals;
            for n in [libc::SIGINT, libc::SIGTERM] {
                let flag = Arc::clone(&signals.signal);
                // Only an atomic store in the signal handler. The waiting thread releases its registration.
                signals.handlers.push(unsafe {
                    signal_hook_registry::register(n, move || flag.store(n, Ordering::Relaxed))?
                });
            }
            Ok(signals)
        }
        #[cfg(not(unix))]
        Ok(signals)
    }
    fn check(&self) -> Result<()> {
        let signal = self.signal.load(Ordering::Relaxed);
        if signal != 0 {
            return Err(crate::automation_cli::RunExit((128 + signal) as u8).into());
        }
        Ok(())
    }
}
impl Drop for WaitSignals {
    fn drop(&mut self) {
        #[cfg(unix)]
        for id in self.handlers.drain(..) {
            signal_hook_registry::unregister(id);
        }
    }
}
fn wait(
    b: &Board,
    actor: &str,
    run: &str,
    after: Option<i64>,
    duration: Duration,
) -> Result<Value> {
    let signals = WaitSignals::install()?;
    Ok(serde_json::to_value(b.swarm_wait(
        actor,
        run,
        after,
        duration,
        || signals.check(),
    )?)?)
}

pub fn run(b: &Board, actor: &str, cmd: Swarm, js: bool) -> Result<()> {
    match cmd {
        Swarm::CheckIn { run, compact } => {
            let run = reference(run.as_deref())?;
            let (target, _) = b.swarm_wait_target(actor, run)?;
            b.swarm_check_in(actor, run, &target)?;
            if compact {
                return output(
                    js,
                    &json!({"run":run,"worker":target,"actor":actor,"status":"working"}),
                    || "Checked in".into(),
                );
            }
            let v = b.swarm_status(actor, run)?;
            output(js, &v, || "Checked in".into())
        }
        Swarm::LaunchFailed {
            worker_name,
            reason,
            run,
        } => {
            let run = reference(run.as_deref())?;
            b.swarm_launch_failed(actor, run, &worker_name, &reason)?;
            let v = b.swarm_status(actor, run)?;
            output(js, &v, || {
                format!("@{worker_name}: launch failed: {reason}")
            })
        }
        Swarm::Receipt { run } => {
            let v = b.swarm_receipt(actor, reference(run.as_deref())?)?;
            output(js, &v, || {
                serde_json::to_string_pretty(&v).expect("receipt serializes")
            })
        }
        Swarm::Attention => {
            let v = b.attention(actor)?;
            output(js, &v, || {
                v.iter()
                    .map(|i| {
                        format!(
                            "{}  {} → {}  {}  respond at #{}",
                            if i.action_required {
                                "ACTION"
                            } else {
                                "FOLLOW-UP"
                            },
                            i.waiting,
                            i.assigned_to,
                            i.summary,
                            i.message_id
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
        Swarm::Decide { id, summary, run } => {
            let v = b.swarm_resolve_decision(actor, reference(run.as_deref())?, id, &summary)?;
            output(js, &v, || format!("Decision recorded at #{}", v.message.id))
        }
        Swarm::Start {
            goal,
            slug,
            repo,
            max_notifications,
            max_active,
            retry_budget,
        } => {
            let v = b.swarm_start_with_policy(
                actor,
                &SwarmStart {
                    goal,
                    slug,
                    repo_root: repo.map(local_path).transpose()?,
                    max_notifications,
                },
                RunPolicy {
                    max_active,
                    retry_budget,
                },
            )?;
            output(js, &v, || v.slug.clone())
        }
        Swarm::Observe {
            run,
            after,
            action_after,
            limit,
        } => {
            let v = b.swarm_observe(
                actor,
                reference(run.as_deref())?,
                after,
                action_after,
                limit,
            )?;
            output(js, &v, || {
                let mut lines = vec![format!(
                    "{}: {} | available slots {} | tasks {:?} | cursor {}",
                    v.run, v.status, v.available_capacity, v.tasks, v.cursor
                )];
                lines.extend(v.actions.iter().map(|a| {
                    format!(
                        "#{} {}{} -> {}: {} ({} waiting)",
                        a.message_id,
                        a.kind,
                        a.worker
                            .as_ref()
                            .map(|w| format!(" @{w}"))
                            .unwrap_or_default(),
                        a.assigned_to,
                        a.summary,
                        a.blocked_dependents
                    )
                }));
                lines.join("\n")
            })
        }
        Swarm::Add {
            worker_name,
            run,
            role,
            brief,
            workspace,
            branch,
        } => {
            let v = b.swarm_add_worker(
                actor,
                reference(run.as_deref())?,
                &SwarmAddWorker {
                    name: worker_name,
                    role_slug: role,
                    brief,
                    workspace_path: workspace.map(local_path).transpose()?,
                    branch_name: branch,
                },
            )?;
            output(js, &v, || v.assembled_prompt.clone())
        }
        Swarm::Status { run } => {
            let v = b.swarm_status(actor, reference(run.as_deref())?)?;
            output(js, &v, || {
                let mut s = format!(
                    "{}  {}\n{}\nNotifications {}/{}",
                    v.slug, v.status, v.goal, v.notifications_sent, v.max_notifications
                );
                for w in &v.workers {
                    s.push_str(&format!(
                        "\n@{}  {}  {}",
                        w.name,
                        w.role_slug.as_deref().unwrap_or("-"),
                        w.status
                    ));
                }
                s
            })
        }
        Swarm::List { before, limit } => {
            let v = b.swarm_list(actor, before, limit)?;
            output(js, &v, || {
                v.iter()
                    .map(|r| format!("{}  {}  {}", r.slug, r.status, r.goal))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
        Swarm::Post {
            mut body,
            run,
            diff,
            schema_data,
            schema_file,
            label,
            key,
            attach,
        } => {
            let run = reference(run.as_deref())?;
            let channel = b.swarm_status(actor, run)?.channel;
            let key = crate::automation_cli::output_key(b, actor, key.as_deref())?;
            let labels = crate::pairs(&label, false)?;
            let mut opts = if let Some(key) = &key
                && !attach.is_empty()
                && match b.posted(actor, key) {
                    Ok(_) => true,
                    Err(e)
                        if board_local::errors::classify(&e).0
                            == board_local::errors::Kind::Missing =>
                    {
                        false
                    }
                    Err(e) => return Err(e),
                } {
                board_local::PostOptions::default()
            } else {
                attachments::placements(b, actor, &channel, &attach, &[])?
            };
            opts.labels = labels;
            opts.key = key;
            let mut text = if schema_data.is_some() || schema_file.is_some() {
                let schema = body
                    .schema
                    .take()
                    .context("--schema is required with --schema-data or --schema-file")?;
                let (name, version) = crate::schema_id(&schema)?;
                let body_uses_stdin = body.text.as_deref() == Some("-")
                    || body.file.as_deref() == Some(std::path::Path::new("-"));
                ensure!(
                    !(body_uses_stdin && schema_file.as_deref() == Some(std::path::Path::new("-"))),
                    "cannot read both message text and --schema-file from stdin"
                );
                let prose = body.read()?;
                let raw_json = match (schema_data, schema_file) {
                    (Some(raw), None) => raw,
                    (None, Some(path)) => crate::load_text(&path)?,
                    _ => unreachable!(),
                };
                let data: Value =
                    serde_json::from_str(&raw_json).context("invalid JSON for block")?;
                board_local::append_block(&prose, name, version, &data)?
            } else {
                body.read()?
            };
            if let Some(range) = diff {
                with_diff(&mut text, &range, &mut opts)?;
            }
            let v = b.swarm_post(actor, run, &text, &opts)?;
            output(js, &v, || {
                if !v.suppressed.is_empty() {
                    format!(
                        "{} (notification limit: {})",
                        v.message.id,
                        v.suppressed.join(", ")
                    )
                } else {
                    v.message.id.to_string()
                }
            })
        }
        Swarm::Dm {
            to,
            text,
            file,
            run,
        } => {
            let text = Body {
                text,
                file,
                schema: None,
            }
            .read()?;
            let v = b.swarm_dm(actor, reference(run.as_deref())?, &to, &text)?;
            output(js, &v, || {
                format!("{}  DM to @{}", v.event_cursor, v.to_worker)
            })
        }
        Swarm::Read { run, after, limit } => {
            let v = b.swarm_read(actor, reference(run.as_deref())?, after, limit, true)?;
            output(js, &v, || {
                let mut lines = vec![];
                for e in &v.entries {
                    if let Some(d) = &e.dm {
                        lines.push(format!(
                            "{}  DM {} -> @{}: {}",
                            e.cursor, d.from_actor, d.to_worker, d.text
                        ));
                    } else if let Some(m) = &e.message {
                        lines.push(format!("{}  {}: {}", e.cursor, m.actor, m.text));
                    }
                }
                lines.push(format!("cursor {}", v.cursor));
                lines.join("\n")
            })
        }
        Swarm::Wait {
            run,
            after,
            timeout,
        } => {
            let dur = timeout
                .parse::<u64>()
                .map(Duration::from_secs)
                .or_else(|_| humantime::parse_duration(&timeout))?;
            let v = wait(b, actor, reference(run.as_deref())?, after, dur)?;
            output(js, &v, || {
                let mut s = format!(
                    "{}  cursor {}",
                    v["status"].as_str().unwrap_or(""),
                    v["cursor"]
                );
                for k in ["messages", "dms"] {
                    for m in v[k].as_array().into_iter().flatten() {
                        if let Some(t) = m["text"].as_str() {
                            s.push_str(&format!("\n{t}"));
                        }
                    }
                }
                s
            })
        }
        Swarm::Done { summary, run } => {
            let v = b.swarm_done(actor, reference(run.as_deref())?, summary.as_deref())?;
            output(js, &v, || format!("@{} {}", v.name, v.status))
        }
        Swarm::Stop { run } => {
            let v = b.swarm_stop(actor, reference(run.as_deref())?)?;
            output(js, &v, || format!("{} {}", v.slug, v.status))
        }
        Swarm::SetLimit { run, limit } => {
            let v = b.swarm_set_limit(actor, reference(run.as_deref())?, limit)?;
            output(js, &v, || {
                format!("{}  limit {}", v.slug, v.max_notifications)
            })
        }
    }
}
