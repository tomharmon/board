use anyhow::{Context, Result, ensure};
use board_local::{
    Board,
    automation::{Task, claim_namespace, recover_claim},
    errors::{Kind, classify},
};
use rusqlite::{Connection, OpenFlags, params};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) struct Cache(Connection);

fn open_private(path: &Path) -> Result<Connection> {
    // A raw file descriptor closed on a live SQLite inode can release this process's POSIX locks.
    board_local::prepare_sqlite_file(path)?;
    let meta = fs::symlink_metadata(path)?;
    ensure!(meta.is_file(), "claim cache is not a regular file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        ensure!(
            meta.permissions().mode() & 0o077 == 0
                && meta.nlink() == 1
                && meta.uid() == unsafe { libc::geteuid() },
            "claim cache file must be private (0600)"
        );
    }
    Ok(Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?)
}

impl Cache {
    pub fn open(board: &Board, db: &Path, actor: &str) -> Result<Self> {
        let db = fs::canonicalize(db)?;
        let dir = std::env::var_os("BOARD_CLAIMS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| db.parent().unwrap_or(Path::new(".")).join(".board-claims"));
        // Resolve ancestors (for example a symlinked temporary directory), never the cache leaf itself.
        let dir = match dir.file_name() {
            Some(leaf) => fs::canonicalize(
                dir.parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new(".")),
            )?
            .join(leaf),
            None => fs::canonicalize(dir)?,
        };
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        if let Err(e) = builder.create(&dir)
            && e.kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(e.into());
        }
        let meta = fs::symlink_metadata(&dir)?;
        ensure!(
            meta.is_dir(),
            "claim cache must be a private directory, not a link"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            // geteuid only reads the effective UID of the current process.
            ensure!(
                meta.permissions().mode() & 0o077 == 0 && meta.uid() == unsafe { libc::geteuid() },
                "claim cache directory must be owner-only (0700)"
            );
        }
        let mut hash = Sha256::new();
        hash.update(db.as_os_str().as_encoded_bytes());
        hash.update([0]);
        hash.update(claim_namespace(board)?);
        hash.update([0]);
        hash.update(actor.as_bytes());
        let path = dir.join(format!("{:x}.db", hash.finalize()));
        let conn = open_private(&path)?;
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.execute_batch("PRAGMA secure_delete=ON; CREATE TABLE IF NOT EXISTS proofs(token TEXT PRIMARY KEY, task INTEGER, touched INTEGER NOT NULL DEFAULT(unixepoch())); DELETE FROM proofs WHERE task IS NULL AND touched<unixepoch()-86400;")?;
        let cache = Self(conn);
        // A sibling process may keep renewing an old proof. Consult the board before pruning it.
        let old = cache
            .0
            .prepare(
                "SELECT token FROM proofs WHERE task IS NOT NULL AND touched<unixepoch()-86400",
            )?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for token in old {
            match recover_claim(board, actor, &token) {
                Ok(t) => cache.remember(&t)?,
                Err(e) if matches!(classify(&e).1, "claim_lost" | "claim_not_found") => {
                    cache.forget(&token)?
                }
                Err(_) => (),
            }
        }
        Ok(cache)
    }
    // Commit the proof before submitting any write, not after the board acknowledges it.
    pub fn prepare(&self, token: Option<&str>) -> Result<String> {
        let token = token
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        board_local::automation::validate_claim_token(&token)?;
        self.0.execute(
            "INSERT INTO proofs(token) VALUES(?) ON CONFLICT(token) DO NOTHING",
            [&token],
        )?;
        Ok(token)
    }
    pub fn remember(&self, t: &Task) -> Result<()> {
        if let Some(token) = &t.claim_token {
            self.0.execute(
                "UPDATE proofs SET task=?,touched=unixepoch() WHERE token=?",
                params![t.id, token],
            )?;
        }
        Ok(())
    }
    pub fn forget(&self, token: &str) -> Result<()> {
        self.0
            .execute("DELETE FROM proofs WHERE token=?", [token])?;
        Ok(())
    }
    pub fn held(&self, b: &Board, actor: &str, id: i64) -> Result<Task> {
        let proof_rows=self.0.prepare("SELECT token FROM proofs WHERE task=? OR task IS NULL ORDER BY (task IS NOT NULL) DESC,touched DESC")?
            .query_map([id],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for token in proof_rows {
            match recover_claim(b, actor, &token) {
                Ok(t) if t.id == id => {
                    self.remember(&t)?;
                    return Ok(t);
                }
                Ok(_) => (),
                Err(e) if classify(&e).1 == "claim_lost" => self.forget(&token)?,
                Err(e) if matches!(classify(&e).1, "claim_not_found" | "claim_conflict") => {}
                Err(e) => return Err(e.context("recover local claim")),
            }
        }
        None.context(Kind::Conflict.error(
            "claim_not_held",
            "no live claim saved for this board, task and actor",
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn cache_files_are_private_regular_inodes_not_links() -> Result<()> {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir()?;
        let parent = fs::canonicalize(dir.path())?;
        let path = parent.join("proofs.db");
        let link = parent.join("alias");
        drop(open_private(&path)?);
        assert_eq!(
            fs::symlink_metadata(&path)?.permissions().mode() & 0o777,
            0o600
        );
        symlink(&path, &link)?;
        assert!(open_private(&link).is_err());
        fs::remove_file(&link)?;
        fs::hard_link(&path, &link)?;
        assert!(open_private(&path).is_err());
        fs::remove_file(link)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640))?;
        assert!(open_private(&path).is_err());
        Ok(())
    }

    #[test]
    fn reopening_a_claim_cache_preserves_another_connections_posix_lock() -> Result<()> {
        const PATH: &str = "BOARD_TEST_CACHE_LOCK_PATH";
        const EXPECT: &str = "BOARD_TEST_CACHE_LOCK_BLOCKED";
        if let Some(path) = std::env::var_os(PATH) {
            let c = open_private(Path::new(&path))?;
            c.busy_timeout(Duration::ZERO)?;
            match (
                std::env::var(EXPECT)?.as_str(),
                c.execute_batch("BEGIN IMMEDIATE"),
            ) {
                ("yes", Err(e)) => assert_eq!(
                    e.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseBusy)
                ),
                ("no", Ok(())) => (),
                (want, got) => anyhow::bail!("expected blocked={want}, observed {got:?}"),
            }
            return Ok(());
        }
        let dir = tempfile::tempdir()?;
        let path = fs::canonicalize(dir.path())?.join("claims.db");
        let c = open_private(&path)?;
        c.execute_batch(
            "CREATE TABLE sentinel(n); BEGIN IMMEDIATE; INSERT INTO sentinel VALUES(1)",
        )?;
        let check = |expect: &str| -> Result<()> {
            let output = Command::new(std::env::current_exe()?)
                .args(["--exact", "automation_cli::cache::tests::reopening_a_claim_cache_preserves_another_connections_posix_lock", "--nocapture"])
                .env(PATH, &path).env(EXPECT, expect).output()?;
            ensure!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed;"),
                "independent writer: {} {}",
                String::from_utf8_lossy(&output.stderr),
                String::from_utf8_lossy(&output.stdout)
            );
            Ok(())
        };
        check("yes")?;
        drop(open_private(&path)?);
        check("yes")?;
        c.execute_batch("COMMIT")?;
        check("no")
    }
}
