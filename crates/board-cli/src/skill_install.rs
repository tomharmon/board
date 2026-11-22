use anyhow::{Context, Result, bail, ensure};
use clap::{Subcommand, ValueEnum};
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
};

const COORDINATOR: &str = include_str!("../assets/board-swarm-SKILL.md");
const V15_COORDINATOR: &str = "126e1fc1455726325dfb5b7b3ad822fe5da73d0fb875e828594131181f3a7394";
const V16_COORDINATOR: &str = "d5c9b58a6134fd9eaf47ff47709425345bf1989810ec2020035a9471312f65de";
const V17_COORDINATOR: &str = "aeefde429f1a99e6215f088492e5cc82fde51abb27aada00192014a28e46bee7";
const V18_COORDINATOR: &str = "1f8dc7910259ea679e342a625199892fa2b190c02b5623c251379cd4d6e95304";
const V19_COORDINATOR: &str = "dacab208ee6cb4e680a95db03491735e1c2da60cea43cb55301af8ede5cab456";
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

fn contents(file: &Path) -> Result<Contents> {
    let m = match fs::symlink_metadata(file) {
        Ok(m) => m,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Contents::Missing),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        m.is_file(),
        "not a regular skill file: {}; inspect it before retrying",
        file.display()
    );
    // The Windows and Unix builds can embed identical instructions with different line endings.
    let mut bytes = Vec::new();
    if m.len() <= 32 * 1024 {
        fs::File::open(file)?
            .take(32 * 1024 + 1)
            .read_to_end(&mut bytes)?;
    }
    if let Ok(text) = std::str::from_utf8(&bytes) {
        let text = text.replace("\r\n", "\n");
        if text == COORDINATOR.replace("\r\n", "\n") {
            return Ok(Contents::Current);
        }
        if matches!(
            format!("{:x}", Sha256::digest(text.as_bytes())).as_str(),
            V15_COORDINATOR | V16_COORDINATOR | V17_COORDINATOR | V18_COORDINATOR | V19_COORDINATOR
        ) {
            return Ok(Contents::Previous);
        }
    }
    Ok(Contents::Custom)
}

fn inspect(file: &Path) -> Result<Contents> {
    let dir = file.parent().expect("skill directory");
    match fs::symlink_metadata(dir) {
        Ok(m) => ensure!(
            m.is_dir(),
            "not a regular skill directory: {}; inspect it before retrying",
            dir.display()
        ),
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Contents::Missing),
        Err(e) => return Err(e.into()),
    }
    let value = contents(file)?;
    if value == Contents::Missing {
        ensure!(
            fs::read_dir(dir)?.next().transpose()?.is_none(),
            "{} has files but no SKILL.md; move the bundle aside before retrying",
            dir.display()
        );
    }
    Ok(value)
}

fn only_instructions(file: &Path) -> Result<()> {
    let dir = file.parent().expect("skill directory");
    for entry in fs::read_dir(dir)? {
        ensure!(
            entry?.file_name() == "SKILL.md",
            "{} has additional files; move the bundle aside and merge your changes after installation",
            dir.display()
        );
    }
    Ok(())
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

pub struct Destination {
    root: PathBuf,
    legacy: Vec<PathBuf>,
}
impl Destination {
    pub fn harness(home: &Path, target: Target) -> Result<Self> {
        let root = match target {
            Target::Claude => claude_dir(home).join("skills"),
            Target::Codex => home.join(".agents/skills"),
            Target::Path => bail!("path requires --dest DIRECTORY"),
        };
        let mut legacy: Vec<PathBuf> = Vec::new();
        if target == Target::Codex {
            // Older Board releases always used ~/.codex, even with a distinct CODEX_HOME.
            for dir in [home.join(".codex/skills"), codex_dir(home).join("skills")] {
                if !same_dir(&dir, &root) && !legacy.iter().any(|p| same_dir(p, &dir)) {
                    legacy.push(dir);
                }
            }
        }
        ensure!(
            root.is_absolute() && legacy.iter().all(|p| p.is_absolute()),
            "harness configuration directory must be absolute"
        );
        ensure!(
            root.to_str().is_some(),
            "harness configuration directory must be a Unicode path"
        );
        Ok(Self { root, legacy })
    }
    fn explicit(target: Target, root: PathBuf) -> Result<Self> {
        if target == Target::Codex
            && let Ok(home) = home()
            && same_dir(&root, &home.join(".agents/skills"))
        {
            return Self::harness(&home, target);
        }
        Ok(Self {
            root,
            legacy: vec![],
        })
    }

    pub fn install(&self, dry: bool) -> Result<(&'static str, PathBuf)> {
        let file = self.root.join(FILE);
        let installed = inspect(&file)?;
        let mut source: Option<(PathBuf, Contents)> = None;
        for old_root in &self.legacy {
            let old = old_root.join(FILE);
            let previous = inspect(&old)?;
            if previous != Contents::Missing {
                ensure!(
                    installed == Contents::Missing,
                    "both {} and {} exist; keep the intended bundle and move the other outside your skill directories before retrying",
                    old.display(),
                    file.display()
                );
                if let Some((earlier, _)) = &source {
                    bail!(
                        "both {} and {} contain a legacy Codex skill; move the intended bundle to {} and the other outside your skill directories before retrying",
                        earlier.display(),
                        old.display(),
                        file.parent().unwrap().display()
                    );
                }
                ensure!(
                    previous != Contents::Custom,
                    "custom legacy Codex skill at {} was left untouched; move its folder to {} before retrying",
                    old.display(),
                    file.parent().unwrap().display()
                );
                source = Some((old, previous));
            }
        }
        match installed {
            Contents::Current => return Ok(("unchanged", file)),
            Contents::Custom => return Ok(("customized", file)),
            Contents::Previous => source = Some((file.clone(), installed)),
            Contents::Missing => (),
        }
        if let Some((old, _)) = &source {
            only_instructions(old)?;
        }
        if dry {
            return Ok((
                if source.is_some() {
                    "would-upgrade"
                } else {
                    "would-install"
                },
                file,
            ));
        }
        let dir = file.parent().unwrap();
        fs::create_dir_all(dir)?;
        ensure!(
            fs::symlink_metadata(dir)?.is_dir(),
            "skill directory changed"
        );
        let mut temp = tempfile::NamedTempFile::new_in(&self.root)?;
        temp.write_all(COORDINATOR.as_bytes())?;
        temp.as_file().sync_all()?;
        let archived = source
            .as_ref()
            .map(|(old, expected)| archive_stock(old, *expected))
            .transpose()?;
        match temp.persist_noclobber(&file) {
            Ok(_) => Ok((
                if archived.is_some() {
                    "upgraded"
                } else {
                    "installed"
                },
                file,
            )),
            Err(e) => {
                if let (Some((old, _)), Some(backup)) = (&source, archived) {
                    // A concurrent install can complete in the gap. Do not re-enable a second
                    // legacy Codex slot or overwrite a user's new instructions.
                    if e.error.kind() == ErrorKind::AlreadyExists
                        && matches!(inspect(&file), Ok(Contents::Current))
                    {
                        return Ok(("upgraded", file));
                    }
                    if old == &file || matches!(contents(&file), Ok(Contents::Missing)) {
                        let _ = fs::hard_link(&backup, old);
                    }
                    bail!(
                        "could not publish {} without replacing another file: {}; original retained at {}. Inspect both before retrying",
                        file.display(),
                        e.error,
                        backup.display()
                    );
                }
                if e.error.kind() == ErrorKind::AlreadyExists {
                    match inspect(&file)? {
                        Contents::Current => return Ok(("unchanged", file)),
                        Contents::Custom => return Ok(("customized", file)),
                        _ => bail!(
                            "{} changed during installation; rerun setup",
                            file.display()
                        ),
                    }
                }
                Err(e.error.into())
            }
        }
    }
}

fn archive_stock(file: &Path, expected: Contents) -> Result<PathBuf> {
    let root = file.parent().and_then(Path::parent).expect("skill root");
    let archive = tempfile::Builder::new()
        .prefix(".board-skill-backup-")
        .tempdir_in(root)?;
    let backup = archive.path().join("coordinator.previous");
    // Rename into a fresh private directory, not over an existing backup. Read the moved
    // bytes again: a writer may have changed them since our initial inspection.
    fs::rename(file, &backup).with_context(|| {
        format!(
            "cannot archive {}; leave the bundle in place and retry",
            file.display()
        )
    })?;
    let _ = archive.keep();
    if !matches!(contents(&backup), Ok(value) if value == expected)
        || !matches!(inspect(file), Ok(Contents::Missing))
    {
        let _ = fs::hard_link(&backup, file);
        bail!(
            "{} changed during upgrade; no replacement installed. Its contents are retained at {}; inspect both before retrying",
            file.display(),
            backup.display()
        );
    }
    Ok(backup)
}

pub fn run(cmd: Skill, js: bool) -> Result<()> {
    let Skill::Install { target, dest } = cmd;
    let dest = match dest {
        Some(p) => Destination::explicit(target, p)?,
        None => Destination::harness(&home()?, target)?,
    };
    ensure!(
        !js || dest.root.to_str().is_some(),
        "the skill path cannot be represented in JSON; choose a Unicode path or omit --json"
    );
    let (state, file) = dest.install(false)?;
    ensure!(
        state != "customized",
        "{} already exists and differs; move it aside or merge the instructions manually",
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
    fn stock_coordinators_upgrade_without_replacing_custom_workflows() {
        let root = tempfile::tempdir().unwrap();
        let dest = Destination::explicit(Target::Path, root.path().into()).unwrap();
        let file = root.path().join(FILE);
        fs::create_dir(file.parent().unwrap()).unwrap();
        for previous in [
            include_bytes!("../tests/fixtures/coordinator-v17.md").as_slice(),
            include_bytes!("../tests/fixtures/coordinator-v18.md").as_slice(),
            include_bytes!("../tests/fixtures/coordinator-v19.md").as_slice(),
        ] {
            fs::write(&file, previous).unwrap();
            assert_eq!(dest.install(false).unwrap().0, "upgraded");
            assert_eq!(fs::read_to_string(&file).unwrap(), COORDINATOR);
        }
        fs::write(&file, b"local instructions").unwrap();
        assert_eq!(dest.install(false).unwrap().0, "customized");
    }

    #[test]
    fn archived_file_is_rechecked_before_replacing_it() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = tmp.path().join(FILE);
        fs::create_dir(slot.parent().unwrap()).unwrap();
        fs::write(
            &slot,
            include_bytes!("../tests/fixtures/coordinator-v15.md"),
        )
        .unwrap();
        let observed = inspect(&slot).unwrap();
        assert!(observed == Contents::Previous);
        fs::write(&slot, b"edited after inspection").unwrap();
        let failure = archive_stock(&slot, observed).unwrap_err().to_string();
        assert!(failure.contains("changed during upgrade"));
        assert_eq!(fs::read(slot).unwrap(), b"edited after inspection");
    }
}
