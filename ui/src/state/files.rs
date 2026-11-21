use super::*;
use anyhow::ensure;
use board_local::{MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS};
use rune::FileDialogOptions;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

fn read_attachment(path: &Path) -> Result<(String, Vec<u8>)> {
    let info = path
        .metadata()
        .with_context(|| format!("Open {}", path.display()))?;
    ensure!(info.is_file(), "Not a regular file: {}", path.display());
    ensure!(
        info.len() <= MAX_ATTACHMENT_BYTES as u64,
        "Attachment exceeds {} MiB",
        MAX_ATTACHMENT_BYTES / (1024 * 1024)
    );
    let name = path
        .file_name()
        .and_then(|p| p.to_str())
        .context("Attachment needs a UTF-8 filename")?;
    let mut bytes = Vec::with_capacity(info.len() as usize);
    File::open(path)?
        .take(MAX_ATTACHMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_ATTACHMENT_BYTES,
        "Attachment grew beyond the limit"
    );
    Ok((name.to_owned(), bytes))
}

impl State {
    fn stage_files(&mut self, paths: &[PathBuf]) -> Result<()> {
        ensure!(
            self.files.len().saturating_add(paths.len()) <= MAX_ATTACHMENTS,
            "At most {MAX_ATTACHMENTS} attachments per message"
        );
        let files = paths
            .iter()
            .map(|path| read_attachment(path))
            .collect::<Result<Vec<_>>>()?;
        self.files.extend(files);
        Ok(())
    }

    pub fn drop_files(&mut self, paths: &[PathBuf], _: &mut Tx) {
        self.error = self.stage_files(paths).err().map(|e| format!("{e:#}"));
    }

    pub fn pick_file(&mut self, tx: &mut Tx) {
        let page = self.page;
        tx.open_file(
            FileDialogOptions {
                title: "Attach file".into(),
                ..Default::default()
            },
            move |s: &mut State, result, _| match result {
                Ok(Some(path)) if s.page == page && s.properties.is_none() => {
                    s.error = s.stage_files(&[path]).err().map(|e| format!("{e:#}"));
                }
                Ok(Some(_)) => s.error = Some("Composer changed; file was not attached".into()),
                Ok(None) => {}
                Err(e) => s.error = Some(e.to_string()),
            },
        );
    }

    pub fn save_attachment(&mut self, id: i64, name: &str, tx: &mut Tx) {
        tx.save_file(
            FileDialogOptions {
                title: "Save attachment".into(),
                file_name: Some(name.into()),
                ..Default::default()
            },
            move |s: &mut State, result, _| match result {
                Ok(Some(path)) => {
                    s.error = s
                        .export_attachment(id, &path)
                        .err()
                        .map(|e| format!("{e:#}"))
                }
                Ok(None) => {}
                Err(e) => s.error = Some(e.to_string()),
            },
        );
    }

    fn export_attachment(&self, id: i64, path: &Path) -> Result<()> {
        let (_, bytes) = self.db()?.download(id)?;
        let parent = path.parent().context("Select a file in a directory")?;
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        tmp.write_all(&bytes)?;
        tmp.as_file().sync_all()?;
        tmp.persist_noclobber(path).with_context(|| {
            format!("Could not save {} (choose a new filename)", path.display())
        })?;
        Ok(())
    }
}
