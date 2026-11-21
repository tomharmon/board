use anyhow::{Context, Result, ensure};
use board_local::{
    Board,
    automation::{Task, claim_namespace, recover_claim},
    errors::{Kind, classify},
};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) struct Cache(Connection);

impl Cache {
    pub fn open(board: &Board, db: &Path, actor: &str) -> Result<Self> {
        let db = fs::canonicalize(db)?;
        let dir = std::env::var_os("BOARD_CLAIMS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| db.parent().unwrap_or(Path::new(".")).join(".board-claims"));
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
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(&path)?;
        let meta = file.metadata()?;
        ensure!(meta.is_file(), "claim cache is not a regular file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            ensure!(
                meta.permissions().mode() & 0o077 == 0 && meta.nlink() == 1,
                "claim cache file must be private (0600)"
            );
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.execute_batch("PRAGMA secure_delete=ON; CREATE TABLE IF NOT EXISTS proofs(token TEXT PRIMARY KEY, task INTEGER, touched INTEGER NOT NULL DEFAULT(unixepoch())); DELETE FROM proofs WHERE task IS NULL AND touched<unixepoch()-86400;")?;
        Ok(Self(conn))
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
                Err(e)
                    if matches!(
                        classify(&e).1,
                        "claim_not_found" | "claim_lost" | "claim_conflict"
                    ) => {}
                Err(e) => return Err(e.context("recover local claim")),
            }
        }
        None.context(Kind::Conflict.error(
            "claim_not_held",
            "no live claim saved for this board, task and actor",
        ))
    }
}
