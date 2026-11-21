use super::{cache::Cache, print, summary};
use anyhow::{Context, Result};
use board_local::{
    Board,
    automation::{
        DeliveryOutcome, Task, TaskAction, change_task, get_watcher, lease_event, renew_delivery,
        settle_delivery,
    },
};
use std::{
    ffi::OsString,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicI32, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Debug)]
pub(crate) struct RunExit(pub u8);
impl std::fmt::Display for RunExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "command exited {}", self.0)
    }
}
impl std::error::Error for RunExit {}

struct Job(Child);
impl Job {
    fn stop(&mut self) {
        #[cfg(unix)]
        {
            // The child leads its own process group. Stop descendants that inherited it too.
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
        }
        #[cfg(not(unix))]
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            self.stop();
        }
    }
}

struct Interrupt {
    signal: Arc<AtomicI32>,
    #[cfg(unix)]
    ids: Vec<signal_hook_registry::SigId>,
}
impl Interrupt {
    fn install() -> Result<Self> {
        let signal = Arc::new(AtomicI32::new(0));
        #[cfg(unix)]
        {
            let mut out = Self {
                signal,
                ids: Vec::new(),
            };
            for n in [libc::SIGINT, libc::SIGTERM] {
                let at = Arc::clone(&out.signal);
                // The callback only performs a lock-free atomic store; cleanup is in the main loop.
                out.ids.push(unsafe {
                    signal_hook_registry::register(n, move || at.store(n, Ordering::SeqCst))?
                });
            }
            Ok(out)
        }
        #[cfg(not(unix))]
        Ok(Self { signal })
    }
}
impl Drop for Interrupt {
    fn drop(&mut self) {
        #[cfg(unix)]
        for id in self.ids.drain(..) {
            signal_hook_registry::unregister(id);
        }
    }
}

fn start(
    argv: &[OsString],
    json: bool,
    db: &Path,
    actor: &str,
    customize: impl FnOnce(&mut Command),
) -> Result<Job> {
    let mut command = Command::new(argv.first().context("no command specified")?);
    command
        .args(&argv[1..])
        .env("BOARD_DB", db)
        .env_remove("BOARD_REQUEST_TOKEN")
        .env_remove("BOARD_CLAIM_TOKEN")
        .env_remove("BOARD_CLAIM_EPOCH")
        .env_remove("BOARD_DELIVERY_RECEIPT");
    let (person, profile) = actor
        .split_once('/')
        .map_or((actor, None), |(p, a)| (p, Some(a)));
    command.env("BOARD_USER", person);
    if let Some(profile) = profile {
        command.env("BOARD_PROFILE", profile);
    } else {
        command.env_remove("BOARD_PROFILE");
    }
    customize(&mut command);
    #[cfg(unix)]
    {
        use std::os::{fd::AsFd, unix::process::CommandExt};
        command.process_group(0);
        if json {
            command.stdout(Stdio::from(std::io::stderr().as_fd().try_clone_to_owned()?));
        }
    }
    #[cfg(not(unix))]
    if json {
        command.stdout(Stdio::piped());
    }
    let mut child = command.spawn().context("start local handler command")?;
    if let Some(mut stdout) = child.stdout.take() {
        thread::spawn(move || {
            let _ = std::io::copy(&mut stdout, &mut std::io::stderr());
        });
    }
    Ok(Job(child))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    b: &Board,
    actor: &str,
    t: &Task,
    lease: i64,
    argv: &[OsString],
    result: Option<&str>,
    json: bool,
    cache: &Cache,
    db: &Path,
) -> Result<()> {
    let token = t
        .claim_token
        .as_deref()
        .context("claim returned no proof")?;
    let change = |op| change_task(b, t.id, actor, Some(t.epoch), Some(token), op);
    let started = || -> Result<(Job, Interrupt)> {
        let interrupt = Interrupt::install()?;
        Ok((
            start(argv, json, db, actor, |c| {
                c.env("BOARD_TASK", t.id.to_string());
            })?,
            interrupt,
        ))
    };
    let (mut job, interrupt) = match started() {
        Ok(v) => v,
        Err(e) => {
            change(TaskAction::Release)?;
            cache.forget(token)?;
            return Err(e);
        }
    };
    let cadence = Duration::from_secs(lease as u64) / 3;
    let mut renewal = Instant::now() + cadence;
    let (finished, code) = loop {
        let sig = interrupt.signal.load(Ordering::SeqCst);
        if sig != 0 {
            job.stop();
            break (change(TaskAction::Release)?, 128 + sig);
        }
        if let Some(status) = job.0.try_wait()? {
            if status.success() {
                break (change(TaskAction::Complete(result))?, 0);
            }
            let (code, reason) = if let Some(code) = status.code() {
                (code, format!("exited {code}"))
            } else {
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    let sig = status.signal().unwrap_or(1);
                    (128 + sig, format!("killed by signal {sig}"))
                }
                #[cfg(not(unix))]
                {
                    (1, "command was interrupted".to_owned())
                }
            };
            break (change(TaskAction::Fail(&reason))?, code);
        }
        if Instant::now() >= renewal {
            // No work should continue once this process cannot prove a live lease.
            if let Err(e) = change(TaskAction::Renew(lease)) {
                job.stop();
                return Err(e);
            }
            renewal = Instant::now() + cadence;
        }
        thread::sleep(Duration::from_millis(50));
    };
    cache.forget(token)?;
    print(&finished, json, || summary(&finished))?;
    if code != 0 {
        return Err(RunExit(code.clamp(1, 255) as u8).into());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn watch(
    b: &Board,
    actor: &str,
    name: &str,
    seconds: i64,
    once: bool,
    argv: &[OsString],
    json: bool,
    db: &Path,
) -> Result<()> {
    use std::io::Write;
    let interrupt = Interrupt::install()?;
    let who = get_watcher(b, name)?
        .run_as
        .unwrap_or_else(|| actor.to_owned());
    loop {
        let sig = interrupt.signal.load(Ordering::SeqCst);
        if sig != 0 {
            return Err(RunExit((128 + sig) as u8).into());
        }
        let Some(d) = lease_event(b, name, seconds)? else {
            if once {
                return print(&serde_json::Value::Null, json, || "nothing pending".into());
            }
            thread::sleep(Duration::from_millis(150));
            continue;
        };
        let cursor = d.event.cursor;
        let bytes = serde_json::to_vec(&d.event)?;
        let started = start(argv, json, db, &who, |c| {
            c.stdin(Stdio::piped())
                .env("BOARD_WATCHER", name)
                .env("BOARD_TRIGGER", &d.trigger)
                .env("BOARD_ATTEMPT", d.attempt.to_string());
        });
        let (code, err) = if let Ok(mut job) = started {
            let mut input = job.0.stdin.take().context("handler stdin not available")?;
            // The handler may choose not to read stdin. Its pipe must not block renewals.
            thread::spawn(move || {
                let _ = input
                    .write_all(&bytes)
                    .and_then(|()| input.write_all(b"\n"));
            });
            let cadence = Duration::from_secs(seconds as u64) / 3;
            let mut renewal = Instant::now() + cadence;
            loop {
                let sig = interrupt.signal.load(Ordering::SeqCst);
                if sig != 0 {
                    job.stop();
                    return Err(RunExit((128 + sig) as u8).into());
                }
                if let Some(status) = job.0.try_wait()? {
                    let code = status.code().unwrap_or(1);
                    break (
                        code,
                        (!status.success()).then(|| format!("handler {status}")),
                    );
                }
                if Instant::now() >= renewal {
                    renew_delivery(b, name, cursor, &d.receipt, seconds)?;
                    renewal = Instant::now() + cadence;
                }
                thread::sleep(Duration::from_millis(50));
            }
        } else {
            (1, Some("could not start watcher handler".to_owned()))
        };
        settle_delivery(
            b,
            name,
            cursor,
            Some(&d.receipt),
            err.as_deref()
                .map_or(DeliveryOutcome::Ack, DeliveryOutcome::Error),
        )?;
        print(
            &serde_json::json!({"watcher":name,"cursor":cursor,"trigger":d.trigger,"attempt":d.attempt,"error":err}),
            json,
            || {
                format!(
                    "{name} {cursor}: {}",
                    err.as_deref().unwrap_or("acknowledged")
                )
            },
        )?;
        if once {
            return if code == 0 {
                Ok(())
            } else {
                Err(RunExit(code.clamp(1, 255) as u8).into())
            };
        }
    }
}
