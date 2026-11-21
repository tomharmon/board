//! A bounded local transport. No media paths, shells or URL handlers.
mod decoder;
mod sound;

pub(super) fn avif_png(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    use std::process::Command;
    let mut file = tempfile::tempfile()?;
    file.write_all(bytes)?;
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-v",
        "error",
        "-nostdin",
        "-threads",
        "1",
        "-filter_threads",
        "1",
        "-max_alloc",
        "33554432",
        "-probesize",
        "4194304",
        "-analyzeduration",
        "2000000",
        "-max_pixels",
        "8000000",
        "-max_streams",
        "8",
        "-protocol_whitelist",
        "fd",
        "-format_whitelist",
        "mov",
        "-codec_whitelist",
        "av1,libdav1d",
        "-enable_drefs",
        "0",
        "-use_absolute_path",
        "0",
        "-f",
        "mov",
        "-i",
        "fd:",
        "-map",
        "0:v:0",
        "-an",
        "-sn",
        "-dn",
        "-vf",
        "scale=w='min(1024,iw)':h='min(1024,ih)':force_original_aspect_ratio=decrease",
        "-frames:v",
        "1",
        "-c:v",
        "png",
        "-pix_fmt",
        "rgba",
        "-threads",
        "1",
        "-f",
        "image2pipe",
        "pipe:1",
    ]);
    decoder::run(cmd, &file, 5 * 1024 * 1024, &AtomicBool::new(false)).map_err(|_| {
        anyhow::anyhow!("AVIF preview needs a supported image and a local FFmpeg decoder")
    })
}

use rune::{Pixels, Tx};
use std::{
    fs::File,
    io::{Seek, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

const W: u32 = 320;
const H: u32 = 180;
pub const FPS: usize = 12;
const FRAMES: usize = 12;
const BYTES: usize = (W * H * 4) as usize;

#[derive(Clone, Copy)]
pub(super) enum Format {
    Mov,
    Webm,
}
impl Format {
    pub(super) fn detect(mime: &str, bytes: &[u8]) -> Option<Self> {
        match mime {
            "video/mp4" | "video/quicktime" if bytes.get(4..8) == Some(b"ftyp") => Some(Self::Mov),
            "video/webm" if decoder::webm_header(bytes) => Some(Self::Webm),
            _ => None,
        }
    }
}

struct Pending {
    first: usize,
    recv: mpsc::Receiver<Result<decoder::Segment, String>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

pub struct Clip {
    source: Arc<File>,
    format: Format,
    frames: Vec<Pixels>,
    first: usize,
    pub current: usize,
    pub total: usize,
    pub playing: bool,
    pub error: Option<String>,
    pub sound: bool,
    pub sound_error: Option<String>,
    pub peak: u8,
    audio_checked: bool,
    pcm: Option<Vec<u8>>,
    sink: Option<sound::Sink>,
    last_sample: Option<usize>,
    pending: Option<Pending>,
    next: f64,
}
impl Clip {
    pub const FIVE_SECONDS: isize = (FPS * 5) as isize;
    pub(super) fn open(format: Format, data: &[u8]) -> std::io::Result<Self> {
        let mut file = tempfile::tempfile()?;
        file.write_all(data)?;
        file.rewind()?;
        Ok(Self {
            source: Arc::new(file),
            format,
            frames: vec![],
            first: 0,
            current: 0,
            total: 0,
            playing: false,
            error: None,
            pending: None,
            next: 0.,
            sound: false,
            sound_error: None,
            peak: 0,
            audio_checked: false,
            pcm: None,
            sink: None,
            last_sample: None,
        })
    }
    pub fn frame(&self) -> Option<&Pixels> {
        self.current
            .checked_sub(self.first)
            .and_then(|i| self.frames.get(i))
    }
    fn request(&mut self) {
        let first = self.current / FRAMES * FRAMES;
        let (total, source, format) = (self.total, Arc::clone(&self.source), self.format);
        let stop = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&stop);
        let (send, recv) = mpsc::channel();
        let sound = (self.sound && self.playing).then_some(!self.audio_checked);
        let worker = thread::spawn(move || {
            let _ = send.send(decoder::segment(
                &source, format, first, total, sound, &cancel,
            ));
        });
        self.pending = Some(Pending {
            first,
            recv,
            stop,
            worker: Some(worker),
        });
    }
    /// None means the placement is no longer readable; the caller drops the entire viewer.
    pub fn poll(
        &mut self,
        now: f64,
        tx: &mut Tx,
        mut authorized: impl FnMut() -> bool,
    ) -> Option<bool> {
        let mut changed = false;
        if let Some(job) = &self.pending {
            let first = job.first;
            let canceled = job.stop.load(Ordering::Relaxed);
            let result = match job.recv.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Disconnected) => Err("Local decoder stopped".into()),
                Err(mpsc::TryRecvError::Empty) => {
                    tx.wake_after(Duration::from_millis(30));
                    return Some(false);
                }
            };
            self.pending = None;
            if !authorized() {
                return None;
            }
            if !canceled && self.current / FRAMES * FRAMES == first {
                match result {
                    Ok(result) => {
                        let total = result.total;
                        self.frames = result
                            .rgba
                            .as_chunks::<BYTES>()
                            .0
                            .iter()
                            .map(|rgba| {
                                let p = Pixels::new();
                                p.set(W, H, rgba.to_vec());
                                p
                            })
                            .collect();
                        self.first = first;
                        self.total = if self.frames.len() < FRAMES {
                            total.min(first + self.frames.len())
                        } else {
                            total
                        };
                        self.current = self.current.min(self.total.saturating_sub(1));
                        self.pcm = None;
                        if self.sound
                            && self.playing
                            && let Some(decoded) = result.pcm
                        {
                            match decoded {
                                Ok(pcm) => {
                                    self.pcm = Some(pcm);
                                    self.audio_checked = true;
                                }
                                Err(e) => self.silence(Some(e)),
                            }
                        }
                    }
                    Err(e) => {
                        self.error = Some(e);
                        self.playing = false;
                        self.stop_audio();
                    }
                }
                self.next = now + 1. / FPS as f64;
            }
            changed = true;
        }
        if self.playing && self.frame().is_some() {
            if self.next == 0. {
                self.next = now + 1. / FPS as f64;
            }
            if now >= self.next {
                if self.current + 1 >= self.total {
                    self.playing = false;
                    self.stop_audio();
                } else {
                    self.current += 1;
                }
                self.next += 1. / FPS as f64;
                changed = true;
            }
            tx.wake_after(Duration::from_millis(35));
        }
        if self.playing
            && self.sound
            && self.pcm.is_some()
            && self.frame().is_some()
            && self.last_sample != Some(self.current)
        {
            if !authorized() {
                return None;
            }
            if let Err(e) = self.audio_frame() {
                self.silence(Some(e));
            }
            changed = true;
        }
        if self.pending.is_none()
            && (self.frame().is_none() || (self.sound && self.playing && self.pcm.is_none()))
            && self.error.is_none()
        {
            if !authorized() {
                return None;
            }
            self.request();
            tx.wake_after(Duration::from_millis(25));
            changed = true;
        }
        Some(changed)
    }
    fn audio_frame(&mut self) -> Result<(), String> {
        let bytes = self.pcm.as_ref().expect("decoded audio");
        let at = (self.current - self.first) * sound::BYTES_PER_FRAME;
        if let Some(frame) = bytes.get(at..bytes.len().min(at + sound::BYTES_PER_FRAME))
            && !frame.is_empty()
        {
            let peak = frame
                .as_chunks::<2>()
                .0
                .iter()
                .map(|w| i16::from_le_bytes(*w).unsigned_abs() as u32)
                .max()
                .unwrap_or(0);
            self.peak = (100 * peak / 32768) as u8;
            if self.sink.is_none() {
                self.sink = Some(sound::Sink::open()?);
            }
            self.sink.as_mut().expect("open sink").write_frame(frame)?;
        }
        self.last_sample = Some(self.current);
        Ok(())
    }
    fn stop_audio(&mut self) {
        self.sink = None;
        self.pcm = None;
        self.last_sample = None;
        self.peak = 0;
    }
    fn silence(&mut self, reason: Option<String>) {
        self.stop_audio();
        self.sound = false;
        self.sound_error = reason;
    }
    pub fn toggle_sound(&mut self) {
        if self.sound {
            self.silence(None);
        } else {
            self.sound_error = None;
            self.sound = true;
        }
    }
    pub fn audio_active(&self) -> bool {
        self.playing && self.sound && self.sink.as_ref().is_some_and(|s| s.written() > 0)
    }
    pub fn seek(&mut self, frame: usize) {
        if self.total == 0 {
            return;
        }
        self.playing = false;
        self.stop_audio();
        self.next = 0.;
        self.error = None;
        self.current = frame.min(self.total - 1);
        if let Some(job) = &self.pending
            && job.first != self.current / FRAMES * FRAMES
        {
            // Only one process at once. poll discards a superseded result before starting the next.
            job.stop.store(true, Ordering::Relaxed);
        }
    }
    pub fn step(&mut self, delta: isize) {
        self.seek(self.current.saturating_add_signed(delta));
    }
    pub fn play(&mut self) {
        if self.total == 0 && self.error.is_some() {
            return;
        }
        if !self.playing && self.total > 0 && self.current + 1 >= self.total {
            self.seek(0);
        }
        self.error = None;
        self.playing = !self.playing;
        self.next = 0.;
        if !self.playing {
            self.stop_audio();
        }
    }
    pub fn time(&self) -> String {
        if self.total == 0 {
            return "Opening video".into();
        }
        let show = |n: usize| {
            let sec = n / FPS;
            format!("{}:{:02}", sec / 60, sec % 60)
        };
        format!(
            "{}   {} / {}   Frame {} of {}",
            if self.sound { "Video" } else { "Silent video" },
            show(self.current),
            show(self.total),
            self.current + 1,
            self.total
        )
    }
}

pub(super) fn visible(board: Option<&board_local::Board>, message: i64, file: i64) -> bool {
    board.is_some_and(|db| {
        db.get(message)
            .is_ok_and(|m| !m.deleted && m.attachments.iter().any(|a| a.id == file))
    })
}
impl super::State {
    pub fn video_control(&mut self, tx: &mut Tx, change: impl FnOnce(&mut Clip)) {
        let Some(p) = &mut self.preview else {
            return;
        };
        if !visible(self.board.as_ref(), p.message, p.file.id) {
            self.preview = None;
            self.refresh();
            return;
        }
        if let super::Preview::Clip(c) = &mut p.content {
            change(c);
            tx.wake_after(Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Format;
    #[test]
    fn webm_needs_an_ebml_header_with_an_actual_webm_doctype() {
        let bytes = include_bytes!("../../tests/fixtures/timeline.webm");
        assert!(Format::detect("video/webm", bytes).is_some());
        assert!(Format::detect("video/mp4", bytes).is_none());
        assert!(
            Format::detect(
                "video/webm",
                include_bytes!("../../tests/fixtures/swatches.mp4")
            )
            .is_none()
        );
        for invalid in [
            b"\x1a\x45\xdf\xa3\x84webm".as_slice(),
            b"\x1a\x45\xdf\xa3\xff\x42\x82\x84webm",
            b"\x1a\x45\xdf\xa3\x8b\x42\x82\x88matroska",
        ] {
            assert!(Format::detect("video/webm", invalid).is_none());
        }
    }
}
