use anyhow::{Context, Result, ensure};
use board_local::{Board, sources::*};
use clap::{Subcommand, ValueEnum};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Subcommand)]
pub enum Sources {
    List,
    /// Show a source's local outbound renderer, install a Rhai program, or clear it.
    Renderer {
        name: String,
        #[arg(long, conflicts_with = "clear")]
        file: Option<PathBuf>,
        #[arg(long)]
        clear: bool,
    },
    /// Map an external repository, Slack channel, Linear team, or local stream to a board channel.
    Add {
        name: String,
        #[arg(value_enum)]
        provider: Kind,
        remote: String,
        channel: String,
    },
    Show {
        name: String,
    },
    /// Turn publishing on or off. Only subsequent local changes to linked threads are queued.
    Publishing {
        name: String,
        #[arg(value_enum)]
        mode: Switch,
    },
    /// Change only the named settings. All mirroring is disabled by default.
    Configure {
        name: String,
        #[arg(long)]
        mirror_roots: Option<bool>,
        #[arg(long)]
        mirror_files: Option<bool>,
        #[arg(long)]
        display_authors: Option<bool>,
        /// Choose which new GitHub conversations this source imports: all, issues, or pulls.
        #[arg(long)]
        github_items: Option<GithubItems>,
    },
}
#[derive(Clone, ValueEnum)]
pub enum Kind {
    Local,
    Github,
    Slack,
    Linear,
}
impl From<Kind> for Provider {
    fn from(k: Kind) -> Self {
        match k {
            Kind::Local => Self::Local,
            Kind::Github => Self::Github,
            Kind::Slack => Self::Slack,
            Kind::Linear => Self::Linear,
        }
    }
}
#[derive(Clone, ValueEnum)]
pub enum Switch {
    On,
    Off,
}
#[derive(Subcommand)]
pub enum Sync {
    /// Show private Slack file transfers that need attention after three unsuccessful pulls.
    FileFailures { name: String },
    /// Explicitly publish a Slack assistant thread status; omit STATUS to clear it.
    Status {
        name: String,
        thread: String,
        #[arg(default_value = "")]
        status: String,
    },
    /// Fetch one private Slack file onto its original, already-imported Board message.
    FetchFile {
        name: String,
        thread: String,
        external_message: String,
        external_file: String,
    },
    /// Explicitly queue one attachment for publishing to a linked Slack conversation.
    ShareFile {
        name: String,
        thread: String,
        #[arg(value_parser=crate::parse_id)]
        message: i64,
        file: String,
    },
    /// Inspect verified file copy IDs for a local message.
    Files {
        name: String,
        #[arg(value_parser=crate::parse_id)]
        message: i64,
    },
    /// Read a configured provider without saving messages or advancing the board cursor.
    Preview {
        name: String,
        thread: Option<String>,
        #[arg(long)]
        channel: Option<String>,
        #[arg(long, conflicts_with = "thread")]
        cursor: Option<String>,
        #[arg(long, default_value_t = 15)]
        limit: usize,
    },
    /// Explicitly join the configured public, non-shared Slack channel. Never performed by pull.
    Join {
        name: String,
        #[arg(long)]
        channel: Option<String>,
        #[arg(long, required = true)]
        confirm: bool,
    },
    /// Enqueue a provider event file offline. KIND is the GitHub event, or 'slack'/'linear'.
    Ingest {
        name: String,
        kind: String,
        delivery: String,
        #[arg(default_value = "-")]
        file: PathBuf,
        /// Verify the provider's original signature with BOARD_SYNC_<NAME>_SIGNING_SECRET.
        #[arg(long)]
        signature: Option<String>,
        /// Slack's X-Slack-Request-Timestamp with a signed Slack event.
        #[arg(long, requires = "signature")]
        timestamp: Option<String>,
    },
    /// Queue a new provider conversation for this local thread and its existing local replies.
    Start {
        name: String,
        #[arg(value_parser=crate::parse_id)]
        root: i64,
    },
    /// Join an existing provider thread locally. Only replies posted after the join may be delivered.
    JoinThread {
        name: String,
        thread: String,
        #[arg(value_parser=crate::parse_id)]
        root: i64,
    },
    /// Queue previously unshared local replies in an already linked thread.
    Backfill { name: String, thread: String },
    /// Fetch selected conversations from the provider. Omit THREAD for the configured repo/channel/team.
    Pull {
        name: String,
        thread: Option<String>,
        #[arg(long)]
        watch: bool,
        #[arg(long, default_value = "5m")]
        interval: String,
    },
    /// Publish at most N queued changes (requires prior opt-in and a provider credential).
    Send {
        name: String,
        #[arg(long, default_value_t = 1)]
        limit: usize,
    },
    /// Apply one normalized JSON envelope immediately; '-' reads stdin.
    Import {
        name: String,
        #[arg(default_value = "-")]
        file: PathBuf,
    },
    /// Process stored incoming events offline.
    Drain {
        name: String,
        #[arg(long, default_value_t = 500)]
        limit: usize,
    },
    /// Record an operator-verified external message copy, not just a conversation link.
    Link {
        name: String,
        thread: String,
        external: String,
        #[arg(value_parser=crate::parse_id)]
        message: i64,
        #[arg(long)]
        url: Option<String>,
    },
    /// Find a board thread or exact reply by external identifiers.
    Find {
        name: String,
        thread: String,
        external: Option<String>,
    },
    /// Find a local message by a previously recorded provider permalink.
    Linked { name: String, url: String },
    /// Find a readable local message and its thread from a permalink, without going online.
    Locate { url: String },
    /// Show public copy addresses and delivery states for one readable message.
    Targets {
        #[arg(value_parser=crate::parse_id)]
        message: i64,
    },
    Copies {
        name: String,
        #[arg(value_parser=crate::parse_id)]
        message: i64,
    },
}
#[derive(Subcommand)]
pub enum Inbox {
    List {
        name: String,
        #[arg(long)]
        outgoing: bool,
        #[arg(long, default_value_t = 0)]
        after: i64,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    Push {
        name: String,
        #[arg(default_value = "-")]
        file: PathBuf,
    },
    Next {
        name: String,
        #[arg(long)]
        outgoing: bool,
        #[arg(long, default_value_t = 60)]
        lease: i64,
    },
    Ack {
        name: String,
        id: i64,
        #[arg(long, env = "BOARD_DELIVERY_RECEIPT", hide_env_values = true)]
        receipt: String,
        #[arg(long)]
        external: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        thread: Option<String>,
    },
    Fail {
        name: String,
        id: i64,
        error: String,
        #[arg(long, env = "BOARD_DELIVERY_RECEIPT", hide_env_values = true)]
        receipt: String,
        #[arg(long)]
        uncertain: bool,
    },
    Resolve {
        name: String,
        id: i64,
        #[arg(value_parser=["retry","skip","delivered"])]
        action: String,
        #[arg(long)]
        external: Option<String>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        thread: Option<String>,
    },
}
fn file<T: DeserializeOwned>(p: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&raw_file(p)?)?)
}
fn raw_file(p: &Path) -> Result<Vec<u8>> {
    let input: Box<dyn Read> = if p == Path::new("-") {
        Box::new(std::io::stdin())
    } else {
        Box::new(File::open(p)?)
    };
    let mut b = Vec::new();
    input.take(2 * 1024 * 1024 + 1).read_to_end(&mut b)?;
    ensure!(b.len() <= 2 * 1024 * 1024, "input exceeds 2 MiB");
    Ok(b)
}
fn print<T: Serialize>(v: &T, js: bool, human: impl FnOnce() -> String) -> Result<()> {
    if js {
        println!("{}", serde_json::to_string(v)?);
    } else {
        println!("{}", crate::safe(&human()));
    }
    Ok(())
}
fn dir(out: bool) -> Direction {
    if out { Direction::Out } else { Direction::In }
}
pub fn sources(b: &Board, a: &str, cmd: Option<Sources>, js: bool) -> Result<()> {
    let source = match cmd.unwrap_or(Sources::List) {
        Sources::Renderer { name, file, clear } => {
            if let Some(p) = file {
                let body = crate::load_text(&p)?;
                b.set_source_renderer(a, &name, Some(&body))?;
            } else if clear {
                b.set_source_renderer(a, &name, None)?;
            }
            let value = b.source_renderer(a, &name)?;
            return print(&json!({"script":value}), js, || {
                value.unwrap_or_else(|| "default renderer".into())
            });
        }
        Sources::List => {
            let list = b.sources(a)?;
            return print(&list, js, || {
                list.iter()
                    .map(|s| {
                        format!(
                            "{}  {:?} {} -> {}  publishing {}",
                            s.name, s.provider, s.remote, s.channel, s.outbound
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            });
        }
        Sources::Add {
            name,
            provider,
            remote,
            channel,
        } => {
            b.create_source(a, &name, provider.into(), &remote, &channel)?
                .0
        }
        Sources::Show { name } => b.source(a, &name)?,
        Sources::Configure {
            name,
            mirror_roots,
            mirror_files,
            display_authors,
            github_items,
        } => b.patch_source(
            a,
            &name,
            &SourcePatch {
                outbound: None,
                mirror_roots,
                mirror_files,
                display_authors,
                github_items,
            },
        )?,
        Sources::Publishing { name, mode } => {
            b.set_outbound(a, &name, matches!(mode, Switch::On))?
        }
    };
    print(&source, js, || {
        format!(
            "{}  {:?} {} -> {}  publishing {}; roots {}, files {}, author names {}",
            source.name,
            source.provider,
            source.remote,
            source.channel,
            source.outbound,
            source.settings.mirror_roots,
            source.settings.mirror_files,
            source.settings.display_authors
        )
    })
}
pub fn sync(b: &Board, a: &str, cmd: Sync, js: bool) -> Result<()> {
    match cmd {
        Sync::FileFailures { name } => {
            let files = b.pending_media(a, &name, true)?;
            print(&files, js, || {
                files
                    .iter()
                    .map(|f| {
                        format!(
                            "{} {} {}: {}",
                            f.thread,
                            f.external,
                            f.file,
                            f.error.as_deref().unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
        Sync::Status {
            name,
            thread,
            status,
        } => {
            let src = b.source(a, &name)?;
            board_sync::Connector::from_env(&src)?.slack_status(b, a, &name, &thread, &status)?;
            print(&json!({"thread":thread,"status":status}), js, || {
                "status updated".into()
            })
        }
        Sync::FetchFile {
            name,
            thread,
            external_message,
            external_file,
        } => {
            let config = b.source(a, &name)?;
            let asset = board_sync::Connector::from_env(&config)?.fetch_file(
                b,
                a,
                &name,
                &thread,
                &external_message,
                &external_file,
            )?;
            print(&asset, js, || format!("{}  {}", asset.key, asset.name))
        }
        Sync::ShareFile {
            name,
            thread,
            message,
            file,
        } => {
            let asset = b.file_id(a, &file)?;
            let j = b.stage_source_file(a, &name, &thread, message, asset)?;
            print(&j, js, || format!("queued file delivery {}", j.id))
        }
        Sync::Files { name, message } => {
            let f = b.source_files(a, &name, message)?;
            print(&f, js, || {
                f.iter()
                    .map(|x| format!("{}  {}", x.asset, x.external))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
        Sync::Preview {
            name,
            thread,
            channel,
            cursor,
            limit,
        } => {
            b.require_owner()?;
            let cfg = b.source(a, &name)?;
            let v = board_sync::Connector::from_env(&cfg)?.preview_target(
                b,
                a,
                &name,
                thread.as_deref(),
                channel.as_deref(),
                cursor.as_deref(),
                limit,
            )?;
            print(&v, js, || {
                serde_json::to_string_pretty(&v).unwrap_or_default()
            })
        }
        Sync::Join {
            name,
            confirm,
            channel,
        } => {
            ensure!(confirm, "joining a public Slack channel requires --confirm");
            b.require_owner()?;
            let cfg = b.source(a, &name)?;
            board_sync::Connector::from_env(&cfg)?.join_slack_target(
                b,
                a,
                &name,
                channel.as_deref(),
            )?;
            let channel = channel.unwrap_or(cfg.remote);
            print(&json!({"channel":channel}), js, || {
                format!("joined {channel}")
            })
        }
        Sync::Ingest {
            name,
            kind,
            delivery,
            file: p,
            signature,
            timestamp,
        } => {
            let cfg = b.source(a, &name)?;
            let body = raw_file(&p)?;
            let var = format!(
                "BOARD_SYNC_{}_SIGNING_SECRET",
                name.replace('-', "_").to_ascii_uppercase()
            );
            let secret = std::env::var(&var).ok();
            if secret.is_some() || signature.is_some() {
                board_sync::verify_intake(
                    &cfg.provider,
                    secret
                        .as_deref()
                        .with_context(|| format!("set {var}"))?
                        .as_bytes(),
                    &body,
                    signature
                        .as_deref()
                        .context("source requires --signature")?,
                    timestamp.as_deref(),
                    chrono::Utc::now().timestamp(),
                )?;
            }
            let id = board_sync::ingest(
                b,
                a,
                &name,
                &kind,
                &delivery,
                serde_json::from_slice(&body)?,
            )?;
            print(&json!({"id":id}), js, || format!("queued {id}"))
        }
        Sync::Start { name, root } => {
            let j = b.start_source_thread(a, &name, root)?;
            print(&j, js, || format!("delivery {}: {}", j.id, j.state))
        }
        Sync::JoinThread { name, thread, root } => {
            let (link, _) = b.join_source_thread(a, &name, &thread, root)?;
            print(&link, js, || {
                format!("thread #{root} linked to {name}:{}", link.thread)
            })
        }
        Sync::Backfill { name, thread } => {
            let n = b.backfill_source(a, &name, &thread)?;
            print(&json!({"queued":n}), js, || {
                format!("queued {n} local replies")
            })
        }
        Sync::Pull {
            name,
            thread,
            watch,
            interval,
        } => {
            let cfg = b.source(a, &name)?;
            let client = board_sync::Connector::from_env(&cfg)?;
            let interval = humantime::parse_duration(&interval)?;
            ensure!(
                !watch || interval.as_secs() >= 5,
                "watch interval must be at least 5s"
            );
            loop {
                let r = client.pull(b, a, &name, thread.as_deref())?;
                print(&r, js, || {
                    format!(
                        "{} threads; {} records, {} applied, {} deferred; {} files, {} file transfers deferred",
                        r.threads, r.records, r.applied, r.failed, r.files, r.files_deferred
                    )
                })?;
                ensure!(
                    r.failed == 0,
                    "one or more imports failed; inspect inbox list {name}"
                );
                if !watch {
                    return Ok(());
                }
                std::thread::sleep(interval);
            }
        }
        Sync::Send { name, limit } => {
            ensure!((1..=500).contains(&limit), "limit must be 1..500");
            let cfg = b.source(a, &name)?;
            let client = board_sync::Connector::from_env(&cfg)?;
            let mut delivered = Vec::new();
            let mut skipped = Vec::new();
            while delivered.len() + skipped.len() < limit {
                let Some(j) = client.send_next(b, a, &name)? else {
                    break;
                };
                if j.state == "done" {
                    delivered.push(j.id);
                } else {
                    skipped.push(j.id);
                }
            }
            print(
                &json!({"delivered":delivered,"skipped":skipped}),
                js,
                || {
                    format!(
                        "delivered {} changes, skipped {}",
                        delivered.len(),
                        skipped.len()
                    )
                },
            )
        }
        Sync::Import { name, file: p } => {
            let m = b.import_source(a, &name, &file(&p)?)?;
            print(&m, js, || m.id.to_string())
        }
        Sync::Drain { name, limit } => {
            let report = b.drain_source_report(a, &name, limit)?;
            print(&report, js, || {
                format!(
                    "{} applied, {} skipped, {} deferred",
                    report.applied, report.skipped, report.failed
                )
            })?;
            ensure!(report.failed == 0, "some imports failed");
            Ok(())
        }
        Sync::Link {
            name,
            thread,
            external,
            message,
            url,
        } => {
            let c = b.attach_source(a, &name, &thread, &external, message, url.as_deref())?;
            print(&c, js, || {
                format!("#{} -> {}/{} {}", c.message, c.source, c.thread, c.external)
            })
        }
        Sync::Find {
            name,
            thread,
            external,
        } => {
            let m = b
                .find_source(a, &name, &thread, external.as_deref())?
                .with_context(|| {
                    board_local::errors::Kind::Missing
                        .error("source_not_found", "no linked conversation")
                })?;
            print(&m, js, || m.id.to_string())
        }
        Sync::Linked { name, url } => {
            let m = b.linked_source(a, &name, &url)?.with_context(|| {
                board_local::errors::Kind::Missing
                    .error("source_not_found", "no recorded permalink")
            })?;
            print(&m, js, || m.id.to_string())
        }
        Sync::Locate { url } => {
            let found = b.locate_link(&url)?;
            print(&found, js, || found.message.to_string())
        }
        Sync::Targets { message } => {
            let found = b.sync_targets(message)?;
            print(&found, js, || {
                if found.targets.is_empty() {
                    return "No sync targets".into();
                }
                found
                    .targets
                    .iter()
                    .map(|t| {
                        format!(
                            "{}  {}  {}  {}",
                            t.source,
                            t.thread.as_deref().unwrap_or(&t.target),
                            t.state,
                            t.url.as_deref().or(t.external.as_deref()).unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
        Sync::Copies { name, message } => {
            let x = b.source_copies(a, &name, message)?;
            print(&x, js, || {
                x.iter()
                    .map(|c| {
                        format!(
                            "{} {} {}",
                            c.thread,
                            c.external,
                            c.url.as_deref().unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
    }
}
pub fn inbox(b: &Board, a: &str, cmd: Inbox, js: bool) -> Result<()> {
    match cmd {
        Inbox::List {
            name,
            outgoing,
            after,
            limit,
        } => {
            let x = b.inbox(a, &name, dir(outgoing), after, limit)?;
            print(&x, js, || {
                x.iter()
                    .map(|j| {
                        format!(
                            "{} {} {}  {}  {}",
                            j.id,
                            j.thread,
                            j.key,
                            j.state,
                            j.error.as_deref().unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
        Inbox::Push { name, file: p } => {
            let e: Envelope = file(&p)?;
            let ids = b.enqueue_imports(a, &name, &[e], None)?;
            print(&ids, js, || ids[0].to_string())
        }
        Inbox::Next {
            name,
            outgoing,
            lease,
        } => {
            let x = b.take_source_delivery(a, &name, dir(outgoing), lease)?;
            print(&x, js, || {
                x.as_ref()
                    .map(|l| format!("{}  {}  receipt: {}", l.job.id, l.job.thread, l.receipt))
                    .unwrap_or_else(|| "nothing pending".into())
            })
        }
        Inbox::Ack {
            name,
            id,
            receipt,
            external,
            url,
            thread,
        } => {
            let r = external.map(|external| DeliveryResult {
                external,
                url,
                thread,
            });
            let m = b.ack_source(a, &name, id, &receipt, r.as_ref())?;
            print(&m, js, || {
                m.as_ref().map_or_else(
                    || format!("acknowledged {id}"),
                    |m| format!("acknowledged {id} -> #{}", m.id),
                )
            })
        }
        Inbox::Fail {
            name,
            id,
            receipt,
            error,
            uncertain,
        } => {
            let j = b.fail_source(
                a,
                &name,
                id,
                &receipt,
                FailureReport {
                    reason: &error,
                    uncertain,
                    retry_after: None,
                },
            )?;
            print(&j, js, || format!("{} {}", j.id, j.state))
        }
        Inbox::Resolve {
            name,
            id,
            action,
            external,
            url,
            thread,
        } => {
            let r = external.map(|external| DeliveryResult {
                external,
                url,
                thread,
            });
            let j = b.resolve_source(a, &name, id, &action, r.as_ref())?;
            print(&j, js, || format!("{} {}", j.id, j.state))
        }
    }
}
