use crate::{attachments, cli::Body, roles_cli::output};
use anyhow::{Context, Result, ensure};
use board_local::{
    Board,
    swarm::{SwarmAddWorker, SwarmStart},
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
    },
    /// Register a worker and render the pinned prompt and environment for your harness.
    Add {
        worker_name: String,
        #[arg(long, env = "BOARD_SWARM_RUN")]
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
        #[arg(env = "BOARD_SWARM_RUN")]
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
        #[arg(long, env = "BOARD_SWARM_RUN")]
        run: Option<String>,
        #[arg(long,num_args=0..=1,default_missing_value="",require_equals=false)]
        diff: Option<String>,
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
        #[arg(long, env = "BOARD_SWARM_RUN")]
        run: Option<String>,
    },
    Read {
        #[arg(long, env = "BOARD_SWARM_RUN")]
        run: Option<String>,
        #[arg(long, default_value_t = 0)]
        after: i64,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Sleep until notified, timeout, or stop. Omit --after to use your saved read position.
    Wait {
        #[arg(long, env = "BOARD_SWARM_RUN")]
        run: Option<String>,
        #[arg(long)]
        after: Option<i64>,
        #[arg(long, default_value = "5m")]
        timeout: String,
    },
    Done {
        summary: Option<String>,
        #[arg(long, env = "BOARD_SWARM_RUN")]
        run: Option<String>,
    },
    Stop {
        #[arg(env = "BOARD_SWARM_RUN")]
        run: Option<String>,
    },
    SetLimit {
        #[arg(long, env = "BOARD_SWARM_RUN")]
        run: Option<String>,
        limit: i64,
    },
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
        Swarm::Start {
            goal,
            slug,
            repo,
            max_notifications,
        } => {
            let v = b.swarm_start(
                actor,
                &SwarmStart {
                    goal,
                    slug,
                    repo_root: repo.map(local_path).transpose()?,
                    max_notifications,
                },
            )?;
            output(js, &v, || v.slug.clone())
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
            body,
            run,
            diff,
            attach,
        } => {
            let run = reference(run.as_deref())?;
            let channel = b.swarm_status(actor, run)?.channel;
            let mut opts = attachments::placements(b, actor, &channel, &attach, &[])?;
            let mut text = body.read()?;
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
