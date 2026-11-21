use crate::Board;
use anyhow::{Context, Result};
use std::path::Path;

impl Board {
    /// Publish a standalone SQLite snapshot only after its online backup has finished.
    pub fn backup(&self, destination: &Path) -> Result<()> {
        self.require_owner()?;
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let file = tempfile::NamedTempFile::new_in(parent)?;
        self.conn.backup(rusqlite::MAIN_DB, file.path(), None)?;
        file.as_file().sync_all()?;
        file.into_temp_path()
            .persist_noclobber(destination)
            .with_context(|| {
                format!(
                    "publish backup {} (will not overwrite)",
                    destination.display()
                )
            })?;
        Ok(())
    }
}
