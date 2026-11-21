use anyhow::{Context, Result, ensure};
use clap::{Subcommand, ValueEnum};
use std::{env, fs, io::Write, path::PathBuf};
const COORDINATOR: &str = include_str!("../assets/board-swarm-SKILL.md");
#[derive(Clone, ValueEnum)]
pub enum Target {
    Claude,
    Codex,
    Path,
}
#[derive(Subcommand)]
pub enum Skill {
    /// Install the coordinator skill. Never overwrites a customized skill.
    Install {
        #[arg(value_enum)]
        target: Target,
        #[arg(long)]
        dest: Option<PathBuf>,
    },
}
pub fn run(cmd: Skill, js: bool) -> Result<()> {
    let Skill::Install { target, dest } = cmd;
    let dest = match dest {
        Some(p) => p,
        None => {
            let sub = match target {
                Target::Claude => ".claude",
                Target::Codex => ".codex",
                Target::Path => anyhow::bail!("path requires --dest DIRECTORY"),
            };
            PathBuf::from(
                env::var_os("HOME")
                    .or_else(|| env::var_os("USERPROFILE"))
                    .context("no home directory; pass --dest")?,
            )
            .join(sub)
            .join("skills")
        }
    };
    let dir = dest.join("board-swarm");
    fs::create_dir_all(&dir)?;
    ensure!(
        fs::symlink_metadata(&dir)?.is_dir(),
        "skill directory may not be a symlink"
    );
    let file = dir.join("SKILL.md");
    let mut temp = tempfile::NamedTempFile::new_in(&dir)?;
    temp.write_all(COORDINATOR.as_bytes())?;
    temp.as_file().sync_all()?;
    match temp.persist_noclobber(&file) {
        Ok(_) => {}
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure!(
                fs::symlink_metadata(&file)?.is_file()
                    && fs::read(&file)? == COORDINATOR.as_bytes(),
                "{} already exists and differs; refusing to overwrite",
                file.display()
            );
        }
        Err(e) => return Err(e.error.into()),
    }
    crate::roles_cli::output(js, &serde_json::json!({"path":file}), || {
        file.display().to_string()
    })
}
