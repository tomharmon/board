use super::*;
use anyhow::ensure;
use board_local::{Attachment, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS};
use image::{ImageFormat, ImageReader, Limits};
use rune::{FileDialogOptions, Pixels};
use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

pub struct AttachmentPreview {
    pub message: i64,
    pub file: Attachment,
    pub content: Preview,
}

pub enum Preview {
    Clip(Clip),
    Image {
        pixels: Pixels,
        width: u32,
        height: u32,
    },
    Text {
        body: String,
        shortened: bool,
    },
    Other(String),
}

fn raster(bytes: &[u8], format: ImageFormat) -> Result<Preview> {
    let mut limits = Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    let reader = || {
        let mut r = ImageReader::with_format(Cursor::new(bytes), format);
        r.limits(limits.clone());
        r
    };
    let (width, height) = reader()
        .into_dimensions()
        .context("Invalid picture header")?;
    ensure!(
        width > 0
            && height > 0
            && width.max(height) <= 4096
            && u64::from(width) * u64::from(height) <= 8_000_000,
        "Picture exceeds the preview size limit"
    );
    let rgba = reader()
        .decode()
        .context("Could not decode picture")?
        .into_rgba8();
    let pixels = Pixels::new();
    pixels.set(width, height, rgba.into_raw());
    Ok(Preview::Image {
        pixels,
        width,
        height,
    })
}

fn preview(file: &Attachment, bytes: &[u8]) -> Preview {
    if matches!(file.content_type.as_str(), "video/mp4" | "video/quicktime")
        && bytes.get(4..8) == Some(b"ftyp")
    {
        return Preview::Clip(Clip::decode(bytes.to_owned()));
    }
    if let Some(format) = image::guess_format(bytes)
        .ok()
        .filter(|f| matches!(f, ImageFormat::Png | ImageFormat::Jpeg))
    {
        return raster(bytes, format).unwrap_or_else(|e| Preview::Other(e.to_string()));
    }
    if matches!(
        file.content_type.as_str(),
        "text/plain" | "text/csv" | "text/markdown" | "application/json"
    ) && let Ok(text) = std::str::from_utf8(bytes)
    {
        let mut body = String::new();
        let mut shortened = false;
        for (n, line) in text.lines().enumerate() {
            if n == 120 || body.len() >= 12 * 1024 {
                shortened = true;
                break;
            }
            if n > 0 {
                body.push('\n');
            }
            for (i, ch) in line.chars().enumerate() {
                if i == 250 {
                    body.push_str("...");
                    shortened = true;
                    break;
                }
                body.push(match ch {
                    '\t' => ' ',
                    c if c.is_control()
                        || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') =>
                    {
                        '\u{fffd}'
                    }
                    c => c,
                });
            }
        }
        return Preview::Text { body, shortened };
    }
    Preview::Other("No inline preview for this file type".into())
}

pub(super) fn read_attachment(path: &Path) -> Result<(String, Vec<u8>)> {
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
    pub fn open_attachment(&mut self, message: i64, id: i64, tx: &mut Tx) {
        let load = || -> Result<AttachmentPreview> {
            // Re-read the live placement before opening, rather than trusting the last frame.
            let db = self.db()?;
            let m = db.get(message)?;
            let file = m
                .attachments
                .into_iter()
                .find(|a| a.id == id && !m.deleted)
                .context("Attachment is no longer on this message")?;
            let content = if file.size > MAX_ATTACHMENT_BYTES as i64 {
                Preview::Other("File exceeds the 4 MiB preview limit".into())
            } else {
                let (_, bytes) = db.download_as(&self.actor, id)?;
                preview(&file, &bytes)
            };
            Ok(AttachmentPreview {
                message,
                file,
                content,
            })
        };
        match load() {
            Ok(p) => {
                if matches!(p.content, Preview::Clip(_)) {
                    tx.wake_after(Duration::from_millis(30));
                }
                self.preview = Some(p);
                self.error = None;
                tx.focus_when_shown("close-preview", true);
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    pub fn close_preview(&mut self, tx: &mut Tx) {
        if let Some(p) = self.preview.take() {
            let key = format!("preview-{}-{}", p.message, p.file.id);
            tx.focus_when_shown(&key, true);
            tx.scroll_into_view(key);
        }
    }

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
        self.files_changed();
        Ok(())
    }

    pub fn drop_files(&mut self, paths: &[PathBuf], _: &mut Tx) {
        self.error = self.stage_files(paths).err().map(|e| format!("{e:#}"));
    }

    pub fn paste_file(&mut self, content: &rune::ClipboardContent) -> bool {
        let Some(image) = content.image() else {
            return false;
        };
        if self.files.len() >= MAX_ATTACHMENTS {
            self.error = Some(format!("At most {MAX_ATTACHMENTS} attachments per message"));
        } else if image.png().len() > MAX_ATTACHMENT_BYTES {
            self.error = Some("Clipboard image exceeds 4 MiB".into());
        } else {
            let name = (1..)
                .map(|n| format!("clipboard-{n}.png"))
                .find(|name| self.files.iter().all(|(f, _)| f != name))
                .expect("bounded queue");
            self.files.push((name, image.png().to_vec()));
            self.files_changed();
            self.error = None;
        }
        true
    }

    pub fn pick_file(&mut self, tx: &mut Tx) {
        let page = self.page;
        let actor = self.actor.clone();
        let generation = std::sync::Arc::downgrade(&self.delivery.instance);
        let (root, target, no_sync) = (self.edits_root(), self.reply_to, self.delivery.no_sync);
        let channel = self.destination.text.clone();
        tx.open_file(
            FileDialogOptions {
                title: "Attach file".into(),
                ..Default::default()
            },
            move |s: &mut State, result, _| match result {
                Ok(Some(path))
                    if generation.upgrade().is_some()
                        && s.page == page
                        && !s.access_blocked
                        && s.actor == actor
                        && s.edits_root() == root
                        && s.reply_to == target
                        && (!root || s.destination.text == channel)
                        && s.delivery.no_sync == no_sync
                        && s.properties.is_none() =>
                {
                    let allowed = || -> Result<()> {
                        if s.delegated {
                            let db = s.db()?;
                            db.activity_seen(&s.actor)?;
                            let destination = if root {
                                channel.trim().to_owned()
                            } else if let Some(id) = page.thread_id() {
                                db.get(target.unwrap_or(id))?.channel
                            } else {
                                anyhow::bail!("No active composer")
                            };
                            if !destination.is_empty() {
                                ensure!(
                                    db.explain(&actor, &destination)?
                                        .contains(&board_local::access::Action::Post),
                                    "Posting to this channel is no longer permitted"
                                );
                            }
                        }
                        Ok(())
                    };
                    s.error = allowed()
                        .and_then(|()| s.stage_files(&[path]))
                        .err()
                        .map(|e| format!("{e:#}"));
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
        let (_, bytes) = self.db()?.download_as(&self.actor, id)?;
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
