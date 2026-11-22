use anyhow::{Context, Result, bail, ensure};
use clap::{Subcommand, ValueEnum};
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
};

const COORDINATOR: &str = include_str!("../assets/board-swarm-SKILL.md");
const RECEIPT: &str = "\n<!-- board-installed-sha256: ";
const FILE: &str = "board-swarm/SKILL.md";

#[derive(Clone, Copy, PartialEq, ValueEnum)]
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

pub fn home() -> Result<PathBuf> {
    #[cfg(windows)]
    let vars = ["USERPROFILE", "HOME"];
    #[cfg(not(windows))]
    let vars = ["HOME", "USERPROFILE"];
    vars.iter()
        .filter_map(env::var_os)
        .map(PathBuf::from)
        .find(|p| p.is_absolute())
        .context("no absolute home directory; set HOME or USERPROFILE")
}
pub fn claude_dir(home: &Path) -> PathBuf {
    env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"))
}
pub fn codex_dir(home: &Path) -> PathBuf {
    env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"))
}

#[derive(Clone, Copy, PartialEq)]
enum Contents {
    Missing,
    Current,
    Previous,
    Custom,
}

// Keep the receipt in the instruction file so it is published together with the body.
fn with_receipt(stock: &str) -> String {
    let body = stock.replace("\r\n", "\n");
    format!("{body}{RECEIPT}{:x} -->\n", Sha256::digest(body.as_bytes()))
}

fn contents(file: &Path, current: &str) -> Result<Contents> {
    let m = match fs::symlink_metadata(file) {
        Ok(m) => m,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Contents::Missing),
        Err(e) => return Err(e.into()),
    };
    ensure!(m.is_file(), "not a regular skill file: {}", file.display());
    let mut bytes = Vec::new();
    if m.len() > 64 * 1024 {
        return Ok(Contents::Custom);
    }
    fs::File::open(file)?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != m.len() {
        return Ok(Contents::Custom);
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(Contents::Custom);
    };
    let text = text.replace("\r\n", "\n");
    if text == current {
        return Ok(Contents::Current);
    }
    if let Some((body, hash)) = text
        .strip_suffix(" -->\n")
        .and_then(|s| s.rsplit_once(RECEIPT))
        && hash == format!("{:x}", Sha256::digest(body.as_bytes()))
    {
        return Ok(Contents::Previous);
    }
    Ok(Contents::Custom)
}

pub struct Destination(PathBuf);

impl Destination {
    pub fn harness(home: &Path, target: Target) -> Result<Self> {
        let root = match target {
            Target::Claude => claude_dir(home).join("skills"),
            Target::Codex => home.join(".agents/skills"),
            Target::Path => bail!("path requires --dest DIRECTORY"),
        };
        ensure!(
            root.is_absolute() && root.to_str().is_some(),
            "harness skill directory must be an absolute Unicode path"
        );
        Ok(Self(root))
    }

    pub fn install(&self, dry: bool) -> Result<(&'static str, PathBuf)> {
        let file = self.0.join(FILE);
        let dir = file.parent().expect("skill directory");
        let current = with_receipt(COORDINATOR);
        let installed = match fs::symlink_metadata(dir) {
            Ok(m) => {
                ensure!(
                    m.is_dir(),
                    "not a regular skill directory: {}",
                    dir.display()
                );
                // A bundle with additional files is user-managed, even if its instructions look stock.
                for entry in fs::read_dir(dir)? {
                    if entry?.file_name() != "SKILL.md" {
                        return Ok(("customized", file));
                    }
                }
                contents(&file, &current)?
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Contents::Missing,
            Err(e) => return Err(e.into()),
        };
        let status = match (installed, dry) {
            (Contents::Current, _) => "unchanged",
            (Contents::Custom, _) => "customized",
            (Contents::Previous, true) => "would-upgrade",
            (Contents::Missing, true) => "would-install",
            (_, false) => {
                fs::create_dir_all(dir)?;
                ensure!(
                    fs::symlink_metadata(dir)?.is_dir(),
                    "skill directory changed"
                );
                publish(&self.0, &file, installed, &current)?;
                if installed == Contents::Missing {
                    "installed"
                } else {
                    "upgraded"
                }
            }
        };
        Ok((status, file))
    }
}

fn publish(root: &Path, file: &Path, expected: Contents, current: &str) -> Result<()> {
    let stage = tempfile::Builder::new()
        .prefix(".board-skill-")
        .tempdir_in(root)?;
    let mut tmp = tempfile::NamedTempFile::new_in(stage.path())?;
    tmp.write_all(current.as_bytes())?;
    tmp.as_file().sync_all()?;
    if expected == Contents::Missing {
        tmp.persist_noclobber(file)?;
        return Ok(());
    }

    // A user can edit the file between inspection and upgrade. Check the actual
    // file moved aside, and never overwrite a competing writer at the destination.
    let old = stage.path().join("previous");
    fs::rename(file, &old)?;
    let result = (|| {
        ensure!(
            contents(&old, current)? == Contents::Previous,
            "skill changed during upgrade"
        );
        tmp.persist_noclobber(file)?;
        Ok(())
    })();
    if let Err(error) = result {
        if fs::hard_link(&old, file).is_err() {
            let _ = stage.keep();
            bail!(
                "could not safely finish the upgrade: {error}; previous skill retained at {}",
                old.display()
            );
        }
        return Err(error);
    }
    Ok(())
}

pub fn run(cmd: Skill, js: bool) -> Result<()> {
    let Skill::Install { target, dest } = cmd;
    let dest = match dest {
        Some(p) => Destination(p),
        None => Destination::harness(&home()?, target)?,
    };
    ensure!(
        !js || dest.0.to_str().is_some(),
        "the skill path cannot be represented in JSON; choose a Unicode path or omit --json"
    );
    let (state, file) = dest.install(false)?;
    ensure!(
        state != "customized",
        "skill bundle at {} is user-managed; move it aside or merge the instructions manually",
        file.display()
    );
    crate::roles_cli::output(
        js,
        &serde_json::json!({"status":state,"path":file.to_string_lossy()}),
        || file.display().to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_edits_are_never_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join(FILE);
        fs::create_dir(file.parent().unwrap()).unwrap();
        fs::write(&file, "user edit").unwrap();
        for observed in [Contents::Missing, Contents::Previous] {
            assert!(publish(root.path(), &file, observed, &with_receipt(COORDINATOR)).is_err());
            assert_eq!(fs::read_to_string(&file).unwrap(), "user edit");
        }
    }
}
