use anyhow::{Context, Result, ensure};
use board_local::{
    Attachment, Board, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS, MAX_DIRECT_BYTES, PostOptions,
};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

struct Input<'a> {
    name: &'a str,
    size: u64,
    file: File,
}

impl<'a> Input<'a> {
    fn open(path: &'a Path) -> Result<Self> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("expected a UTF-8 filename")?;
        board_local::validate_attachment_name(name)?;
        ensure!(
            fs::metadata(path)?.is_file(),
            "expected a regular file: {}",
            path.display()
        );
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file() && metadata.len() <= MAX_DIRECT_BYTES as u64,
            "expected a regular file up to 100 MiB: {name}"
        );
        Ok(Self {
            name,
            size: metadata.len(),
            file,
        })
    }

    fn inline(&mut self) -> Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(self.size as usize);
        (&mut self.file)
            .take(self.size + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 == self.size,
            "file size changed while reading {}",
            self.name
        );
        Ok(bytes)
    }

    fn store(
        mut self,
        board: &Board,
        actor: &str,
        channel: &str,
        mime: &str,
    ) -> Result<Attachment> {
        if self.size <= MAX_ATTACHMENT_BYTES as u64 {
            let data = self.inline()?;
            return board.upload(actor, channel, self.name, mime, &data);
        }
        let mut upload =
            board.start_upload(actor, channel, self.name, mime, self.size as i64, None)?;
        let mut buf = vec![0; MAX_ATTACHMENT_BYTES];
        while upload.received < upload.size {
            let n = buf.len().min((upload.size - upload.received) as usize);
            self.file
                .read_exact(&mut buf[..n])
                .context("file changed while uploading")?;
            upload = board.upload_part(actor, &upload.key, upload.received, &buf[..n])?;
        }
        ensure!(
            self.file.read(&mut buf[..1])? == 0,
            "file grew while uploading {}",
            self.name
        );
        Ok(board.finish_upload(actor, &upload.key)?.0)
    }
}

pub fn upload(
    board: &Board,
    actor: &str,
    channel: &str,
    path: &Path,
    mime: &str,
) -> Result<Attachment> {
    Input::open(path)?.store(board, actor, channel, mime)
}

/// Check every filename, handle and size before writing the first staged byte.
pub fn placements(
    board: &Board,
    actor: &str,
    target: &str,
    paths: &[PathBuf],
    refs: &[String],
) -> Result<PostOptions> {
    ensure!(
        paths.len() + refs.len() <= MAX_ATTACHMENTS,
        "at most ten attachments per message"
    );
    let inputs = paths
        .iter()
        .map(|p| Input::open(p))
        .collect::<Result<Vec<_>>>()?;
    let refs = refs
        .iter()
        .map(|id| board.file_id(actor, id))
        .collect::<Result<Vec<_>>>()?;
    let mut opts = PostOptions::default();
    if inputs.iter().any(|f| f.size > MAX_ATTACHMENT_BYTES as u64) {
        let channel = match crate::message_id(target) {
            Some(id) => board.get(id)?.channel,
            None => target.to_string(),
        };
        for input in inputs {
            let mime = board_local::content_type_for(input.name);
            opts.attachment_ids
                .push(input.store(board, actor, &channel, mime)?.id);
        }
    } else {
        for mut input in inputs {
            opts.files.push((input.name.into(), input.inline()?));
        }
    }
    opts.attachment_ids.extend(refs);
    Ok(opts)
}
