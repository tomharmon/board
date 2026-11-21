use super::{BTreeMap, Path, Result, check_slug};
use anyhow::{Context, ensure};
use serde::Deserialize;
use std::{fs, io::Read, path::Component};

const FILE_LIMIT: usize = 256 * 1024;
const BUNDLE_LIMIT: usize = 1024 * 1024;

fn relative(p: &Path) -> Result<String> {
    ensure!(p.components().all(|c| matches!(c,Component::Normal(s) if s.to_str().is_some_and(|s|
        !s.is_empty() && !s.starts_with('.') && !s.contains(['\\', ':']) && !s.chars().any(char::is_control)))),
        "skill paths must be relative, visible UTF-8 names without traversal or backslashes");
    Ok(p.iter()
        .map(|p| p.to_str().expect("checked above"))
        .collect::<Vec<_>>()
        .join("/"))
}
#[derive(Deserialize, Default)]
struct Metadata {
    name: Option<String>,
    description: Option<String>,
}

pub(super) fn read(root: &Path) -> Result<(String, String, BTreeMap<String, String>)> {
    ensure!(
        fs::symlink_metadata(root)?.is_dir(),
        "select a real skill directory, not a symlink"
    );
    let root = root.canonicalize().context("open skill directory")?;
    let mut pending = vec![root.clone()];
    let mut files = BTreeMap::new();
    let mut total = 0;
    let mut entries = 0;
    while let Some(dir) = pending.pop() {
        ensure!(
            dir.canonicalize()?.starts_with(&root),
            "skill directory left its root"
        );
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let rel = relative(path.strip_prefix(&root)?)?;
            entries += 1;
            ensure!(
                rel.len() <= 1024 && entries <= 128,
                "skill directory too large"
            );
            let ty = entry.file_type()?;
            if ty.is_dir() {
                pending.push(path);
                continue;
            }
            ensure!(
                ty.is_file(),
                "skill bundles may contain only text files, not links or devices: {rel}"
            );
            ensure!(files.len() < 32, "at most 32 files per skill");
            let metadata = entry.metadata()?;
            ensure!(
                metadata.len() <= FILE_LIMIT as u64,
                "skill file exceeds 256 KiB: {rel}"
            );
            let mut opts = fs::OpenOptions::new();
            opts.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.custom_flags(
                    rustix::fs::OFlags::NOFOLLOW.bits() as i32
                        | rustix::fs::OFlags::NONBLOCK.bits() as i32,
                );
            }
            let f = opts.open(&path)?;
            ensure!(
                f.metadata()?.is_file() && path.canonicalize()?.starts_with(&root),
                "skill file left its root"
            );
            let mut data = Vec::new();
            f.take((FILE_LIMIT + 1) as u64).read_to_end(&mut data)?;
            total += data.len();
            ensure!(
                data.len() <= FILE_LIMIT && total <= BUNDLE_LIMIT,
                "skill bundle exceeds size limit"
            );
            let data = String::from_utf8(data).context("skill files must be UTF-8 text")?;
            ensure!(!data.contains('\0'), "binary file in skill bundle: {rel}");
            files.insert(rel, data);
        }
    }
    let md = files
        .get("SKILL.md")
        .context("bundle must contain SKILL.md at its root")?;
    ensure!(!md.trim().is_empty(), "SKILL.md is empty");
    let normalized = md.replace("\r\n", "\n");
    let meta: Metadata = if let Some(rest) = normalized.strip_prefix("---\n") {
        let end = rest
            .split_inclusive('\n')
            .scan(0usize, |offset, line| {
                let here = *offset;
                *offset += line.len();
                Some((here, line.trim_end_matches('\n')))
            })
            .find(|(_, l)| *l == "---")
            .context("unterminated SKILL.md frontmatter")?
            .0;
        serde_yaml_ng::from_str(&rest[..end]).context("invalid SKILL.md frontmatter")?
    } else {
        Metadata::default()
    };
    let slug = meta
        .name
        .or_else(|| root.file_name()?.to_str().map(str::to_owned))
        .context("skill has no valid name")?;
    check_slug(&slug)?;
    let desc = meta.description.unwrap_or_default();
    ensure!(desc.len() <= 4096, "skill description exceeds 4096 bytes");
    Ok((slug, desc, files))
}
