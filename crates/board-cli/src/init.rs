mod config;
mod probe;
use crate::skill_install::{self, Target};
use anyhow::{Context, Result, bail, ensure};
use clap::{Args, ValueEnum};
use serde::Serialize;
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Harness {
    Codex,
    Claude,
}
impl Harness {
    fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
    fn target(self) -> Target {
        match self {
            Self::Codex => Target::Codex,
            Self::Claude => Target::Claude,
        }
    }
}
#[derive(Args)]
pub struct Init {
    /// Report detections and planned changes. Do not run either harness or write any files.
    #[arg(long)]
    dry_run: bool,
    /// Limit configuration to this harness (repeatable or comma-separated).
    #[arg(long, value_enum, value_delimiter = ',')]
    only: Vec<Harness>,
    /// Stable, absolute CLI path for user-scoped registration. Defaults to this installed Board.
    #[arg(long, value_name = "PATH")]
    board_bin: Option<PathBuf>,
}
#[derive(Clone, Copy, PartialEq)]
enum Saved {
    Missing,
    Same,
    Conflict,
}
#[derive(Serialize)]
struct Entry {
    name: &'static str,
    detected: bool,
    registration: &'static str,
    skill: &'static str,
    skill_path: Option<PathBuf>,
    verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}
impl Entry {
    fn new(h: Harness, present: bool) -> Self {
        Self {
            name: h.name(),
            detected: present,
            registration: "not-installed",
            skill: "not-installed",
            skill_path: None,
            verified: false,
            error: None,
        }
    }
}

fn configure(
    h: Harness,
    harness: &Path,
    exe: &Path,
    home: &Path,
    scratch: Option<&Path>,
    e: &mut Entry,
) -> Result<()> {
    let reg = config::registration(h, home, exe)?;
    ensure!(
        reg != Saved::Conflict,
        "an existing or disabled 'board' MCP entry differs; review it in {}. Nothing was replaced",
        h.name()
    );
    let dest = skill_install::Destination::harness(home, h.target())?;
    let (planned, path) = dest.install(true)?;
    e.skill = planned;
    e.skill_path = Some(path);
    if let Some(dir) = scratch {
        if reg == Saved::Missing {
            let mut cmd = Command::new(harness);
            cmd.current_dir(dir);
            match h {
                Harness::Codex => {
                    cmd.args(["mcp", "add", "board", "--"]);
                }
                Harness::Claude => {
                    cmd.args([
                        "mcp",
                        "add",
                        "--scope",
                        "user",
                        "--transport",
                        "stdio",
                        "board",
                        "--",
                    ])
                    .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
                    .env("DISABLE_AUTOUPDATER", "1");
                }
            }
            // Batch shims use cmd.exe. Avoid expanding a literal path through shell metacharacters.
            #[cfg(windows)]
            if harness
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("cmd") || x.eq_ignore_ascii_case("bat"))
            {
                ensure!(
                    !exe.to_string_lossy()
                        .contains(['%', '!', '^', '&', '|', '<', '>', '"']),
                    "use the native harness .exe or manual registration for this install path"
                );
            }
            cmd.arg(exe).arg("mcp");
            probe::invoke(cmd, None)
                .with_context(|| format!("{} user-scoped registration failed", h.name()))?;
        }
        ensure!(
            config::registration(h, home, exe)? == Saved::Same,
            "harness did not save the expected user-scoped command"
        );
        e.registration = if reg == Saved::Same {
            "unchanged"
        } else {
            "registered"
        };
        (e.skill, _) = dest.install(false)?;
        e.verified = true;
    } else {
        e.registration = if reg == Saved::Same {
            "unchanged"
        } else {
            "would-register"
        };
    }
    Ok(())
}
pub fn run(opts: Init, js: bool) -> Result<()> {
    let home = skill_install::home()?;
    ensure!(
        home.to_str().is_some(),
        "harness setup requires a UTF-8 home directory"
    );
    for key in ["CODEX_HOME", "CLAUDE_CONFIG_DIR"] {
        ensure!(
            std::env::var_os(key).is_none_or(|v| v.to_str().is_some()),
            "{key} must be a UTF-8 path for harness setup"
        );
    }
    let exe = config::installed_path(opts.board_bin)?;
    let selected: Vec<_> = [Harness::Codex, Harness::Claude]
        .into_iter()
        .filter(|h| opts.only.is_empty() || opts.only.contains(h))
        .map(|h| (h, config::on_path(h.name()).next()))
        .collect();
    let scratch = if !opts.dry_run && selected.iter().any(|(_, p)| p.is_some()) {
        let dir = tempfile::tempdir()?;
        probe::verify(&exe, dir.path())
            .context("MCP handshake failed before changing any harness settings")?;
        Some(dir)
    } else {
        None
    };
    let mut entries = vec![];
    for (h, p) in selected {
        let mut e = Entry::new(h, p.is_some());
        if let Some(p) = p
            && let Err(err) = configure(
                h,
                &p,
                &exe,
                &home,
                scratch.as_ref().map(|d| d.path()),
                &mut e,
            )
        {
            e.registration = "needs-attention";
            e.error = Some(format!("{err:#}"));
        }
        entries.push(e);
    }
    let ok = entries.iter().all(|e| e.error.is_none());
    let manual = json!({"transport":"stdio","command":exe,"args":["mcp"],"skill":"board skill install path --dest <user-skills-directory>"});
    if js {
        println!(
            "{}",
            json!({"ok":ok,"dry_run":opts.dry_run,"harnesses":entries,"manual":manual})
        );
    } else {
        for e in entries {
            if !e.detected {
                println!("{}: not detected on PATH", e.name);
                continue;
            }
            println!(
                "{}: MCP {}; skill {}{}{}",
                e.name,
                e.registration,
                e.skill,
                e.skill_path
                    .map(|p| format!(" ({})", p.display()))
                    .unwrap_or_default(),
                if e.verified {
                    "; verified swarm_start"
                } else {
                    ""
                }
            );
            if e.skill == "customized" {
                println!(
                    "  Your customized skill was preserved; check its worker identity instructions before using it."
                );
            }
            if let Some(err) = e.error {
                eprintln!("  {err}");
            }
        }
        println!(
            "Other harnesses: register stdio command {:?} with arguments [\"mcp\"]. Install the coordinator skill with: board skill install path --dest <user-skills-directory>",
            exe
        );
        if !opts.dry_run && ok && scratch.is_some() {
            println!(
                "Open a new coding chat and invoke board-swarm. Launch Board from your applications menu or run board ui."
            );
        }
    }
    if !ok {
        bail!("one or more harnesses need attention (see the init report)");
    }
    Ok(())
}
