use super::*;
use crate::roles::{PinnedSkill, SWARM_WORKER_PROTOCOL};
use std::{
    fs,
    io::Write,
    path::{Component, Path},
};

fn private_dir(path: &Path) -> Result<()> {
    let mut opts = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        opts.mode(0o700);
    }
    match opts.create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir(),
        "skill directory must not be a symlink: {}",
        path.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            meta.permissions().mode() & 0o077 == 0,
            "skill directory must be private (0700): {}",
            path.display()
        );
    }
    Ok(())
}
fn safe_file(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && !path.contains(['\\', '\0'])
            && !path.split('/').any(|p| p.is_empty() || p.starts_with('.'))
            && Path::new(path)
                .components()
                .all(|p| matches!(p, Component::Normal(_))),
        "invalid pinned skill path"
    );
    Ok(())
}
impl Board {
    fn swarm_data_dir(&self) -> Result<PathBuf> {
        let db = self
            .conn
            .path()
            .context("cannot export an unknown or non-UTF-8 Board database path to a worker")?;
        if !db.is_empty() {
            let mut path = fs::canonicalize(db)?.into_os_string();
            path.push(".data");
            let path = PathBuf::from(path);
            private_dir(&path)?;
            Ok(path)
        } else {
            if self.swarm_scratch.get().is_none() {
                let _ = self.swarm_scratch.set(tempfile::tempdir()?);
            }
            Ok(self
                .swarm_scratch
                .get()
                .context("temporary swarm storage not initialized")?
                .path()
                .to_path_buf())
        }
    }
    pub(super) fn materialize_spawn(
        &self,
        run: &SwarmRun,
        worker: SwarmWorker,
        prompt: &str,
        skills: &[PinnedSkill],
    ) -> Result<SpawnedWorker> {
        let base = self.swarm_data_dir()?.join("swarms");
        private_dir(&base)?;
        let run_dir = base.join(&run.slug);
        private_dir(&run_dir)?;
        let final_dir = run_dir.join(&worker.name);
        let stage = tempfile::Builder::new()
            .prefix(".register-")
            .tempdir_in(&run_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(stage.path(), fs::Permissions::from_mode(0o700))?;
        }
        let dest = stage.path().join("skills");
        private_dir(&dest)?;
        for skill in skills {
            ensure!(valid_slug(&skill.slug, 64), "invalid pinned skill slug");
            ensure!(
                skill.files.contains_key("SKILL.md"),
                "pinned skill lacks SKILL.md"
            );
            let dir = dest.join(&skill.slug);
            private_dir(&dir)?;
            for (name, text) in &skill.files {
                safe_file(name)?;
                let path = dir.join(name);
                let mut parent = dir.clone();
                for part in Path::new(name)
                    .parent()
                    .unwrap_or(Path::new(""))
                    .components()
                {
                    parent.push(part);
                    private_dir(&parent)?;
                }
                let mut opts = fs::OpenOptions::new();
                opts.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    opts.mode(0o600);
                }
                opts.open(path)?.write_all(text.as_bytes())?;
            }
        }
        // Local export is repeatable; registration is already durable at this point.
        // Never follow links or replace user data at a pre-existing destination.
        match fs::symlink_metadata(&final_dir) {
            Ok(_) => ensure!(
                same_tree(stage.path(), &final_dir)?,
                "existing worker skill directory differs from the pinned assignment: {}",
                final_dir.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Err(e) = fs::rename(stage.path(), &final_dir) {
                    // Another local exporter can finish after the existence check.
                    if !same_tree(stage.path(), &final_dir).unwrap_or(false) {
                        return Err(e.into());
                    }
                }
            }
            Err(e) => return Err(e.into()),
        }
        let skills_dir = final_dir.join("skills");
        let mut assembled_prompt = format!(
            "{SWARM_WORKER_PROTOCOL}\n\nRun: {}\nGoal: {}\nWorker: @{}\nBoard actor: {}\n\nRole instructions:\n{}\n\nAssignment:\n{}",
            run.slug, run.goal, worker.name, worker.actor, prompt, worker.brief
        );
        if let Some(v) = &run.repo_root {
            assembled_prompt.push_str(&format!("\nRepository: {v}"));
        }
        if let Some(v) = &worker.workspace_path {
            assembled_prompt.push_str(&format!("\nWorkspace: {v}"));
        }
        if let Some(v) = &worker.branch_name {
            assembled_prompt.push_str(&format!("\nBranch: {v}"));
        }
        for skill in skills {
            assembled_prompt.push_str(&format!(
                "\n\nSkill: {} (v{})\nLocal files: {}\n{}",
                skill.slug,
                skill.version,
                skills_dir.join(&skill.slug).display(),
                skill.files["SKILL.md"]
            ));
        }
        let selector = format!("{}/{}", run.slug, worker.name);
        let mut env_vars = BTreeMap::from([
            (
                "BOARD_USER".into(),
                worker.actor.split('/').next().unwrap_or("").into(),
            ),
            ("BOARD_PROFILE".into(), worker.name.clone()),
            ("BOARD_SWARM_RUN".into(), run.slug.clone()),
            ("BOARD_WORKER".into(), selector.clone()),
        ]);
        let cli_args = if let Some(db) = self.conn.path().filter(|p| !p.is_empty()) {
            let db = fs::canonicalize(db)?
                .to_str()
                .context("Board path is not UTF-8")?
                .to_owned();
            env_vars.insert("BOARD_DB".into(), db.clone());
            let args = vec!["--db".to_owned(), db, "--worker".into(), selector];
            assembled_prompt.push_str(&format!("\n\nBoard CLI: prepend these literal arguments to each subcommand: {}. Use this assignment's BOARD_USER. Run whoami --json before writing; expect actor {} and run {}. Never use the chat's shared Board MCP connection. On a dedicated worker MCP confirm this exact actor and run with worker_assignment. If neither transport is available, report the blocker rather than speaking as the coordinator.",serde_json::to_string(&args)?,worker.actor,run.slug));
            Some(args)
        } else {
            None
        };
        Ok(SpawnedWorker {
            worker,
            skills_dir,
            env_vars,
            cli_args,
            assembled_prompt,
        })
    }
}
fn same_tree(expected: &Path, actual: &Path) -> Result<bool> {
    let a = fs::symlink_metadata(actual)?;
    let e = fs::symlink_metadata(expected)?;
    if a.file_type() != e.file_type() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if a.permissions().mode() & 0o077 != 0 {
            return Ok(false);
        }
    }
    if e.is_file() {
        return Ok(a.len() == e.len() && fs::read(expected)? == fs::read(actual)?);
    }
    if !e.is_dir() {
        return Ok(false);
    }
    let count = fs::read_dir(actual)?.count();
    let expected_rows = fs::read_dir(expected)?.collect::<std::io::Result<Vec<_>>>()?;
    if count != expected_rows.len() {
        return Ok(false);
    }
    for entry in expected_rows {
        if !same_tree(&entry.path(), &actual.join(entry.file_name()))? {
            return Ok(false);
        }
    }
    Ok(true)
}
