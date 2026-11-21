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
    },
    /// Explicitly join the configured public, non-shared Slack channel. Never performed by pull.
    Join {
        name: String,
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
    },
    /// Queue a new provider conversation for this local thread and its existing local replies.
    Start {
        name: String,
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
    /// Record a verified external copy or explicitly join a local root to an external thread.
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
    let input: Box<dyn Read> = if p == Path::new("-") {
        Box::new(std::io::stdin())
    } else {
        Box::new(File::open(p)?)
    };
    let mut b = Vec::new();
    input.take(2 * 1024 * 1024 + 1).read_to_end(&mut b)?;
    ensure!(b.len() <= 2 * 1024 * 1024, "input exceeds 2 MiB");
    Ok(serde_json::from_slice(&b)?)
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
        Sources::Publishing { name, mode } => {
            b.set_outbound(a, &name, matches!(mode, Switch::On))?
        }
    };
    print(&source, js, || {
        format!(
            "{}  {:?} {} -> {}  publishing {}",
            source.name, source.provider, source.remote, source.channel, source.outbound
        )
    })
}
pub fn sync(b: &Board, a: &str, cmd: Sync, js: bool) -> Result<()> {
    match cmd {
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
        Sync::Preview { name, thread } => {
            b.require_owner()?;
            let cfg = b.source(a, &name)?;
            let v =
                board_sync::Connector::from_env(&cfg)?.preview(b, a, &name, thread.as_deref())?;
            print(&v, js, || {
                serde_json::to_string_pretty(&v).unwrap_or_default()
            })
        }
        Sync::Join { name, confirm } => {
            ensure!(confirm, "joining a public Slack channel requires --confirm");
            b.require_owner()?;
            let cfg = b.source(a, &name)?;
            board_sync::Connector::from_env(&cfg)?.join_public_slack(b, a, &name)?;
            print(&json!({"channel":cfg.remote}), js, || {
                format!("joined {}", cfg.remote)
            })
        }
        Sync::Ingest {
            name,
            kind,
            delivery,
            file: p,
        } => {
            let id = board_sync::ingest(b, a, &name, &kind, &delivery, file(&p)?)?;
            print(&json!({"id":id}), js, || format!("queued {id}"))
        }
        Sync::Start { name, root } => {
            let j = b.start_source_thread(a, &name, root)?;
            print(&j, js, || format!("delivery {}: {}", j.id, j.state))
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
                        "{} threads; {} records, {} applied, {} deferred",
                        r.threads, r.records, r.applied, r.failed
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
            while delivered.len() < limit {
                let Some(j) = client.send_next(b, a, &name)? else {
                    break;
                };
                delivered.push(j.id);
            }
            print(&json!({"delivered":delivered}), js, || {
                format!("delivered {} changes", delivered.len())
            })
        }
        Sync::Import { name, file: p } => {
            let m = b.import_source(a, &name, &file(&p)?)?;
            print(&m, js, || m.id.to_string())
        }
        Sync::Drain { name, limit } => {
            let (n, f) = b.drain_source(a, &name, limit)?;
            print(&json!({"applied":n,"failed":f}), js, || {
                format!("{n} applied, {f} deferred")
            })?;
            ensure!(f == 0, "some imports failed");
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
            let m = b.finish_source(a, &name, id, &receipt, r.as_ref())?;
            print(&m, js, || format!("acknowledged {id} -> #{}", m.id))
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
