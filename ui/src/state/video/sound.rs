//! Only decoded mono PCM can enter the optional local audio sink.
use std::{
    io::Write,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

pub(super) const RATE: usize = 24_000;
pub(super) const BYTES_PER_FRAME: usize = RATE * 2 / super::FPS;

pub(super) struct Sink {
    child: Child,
    send: Option<mpsc::SyncSender<Vec<u8>>>,
    writer: Option<thread::JoinHandle<()>>,
    written: Arc<AtomicUsize>,
}
impl Sink {
    pub fn open() -> Result<Self, String> {
        let mut cmd = Command::new("ffplay");
        cmd.env_clear();
        for key in ["PATH", "SystemRoot"] {
            if let Some(v) = std::env::var_os(key) {
                cmd.env(key, v);
            }
        }
        // Don't inherit arbitrary SDL, Pulse, PipeWire or media URLs from an environment.
        // Linux output must resolve to a real local Unix socket. No automatic TCP/ALSA fallback.
        if std::env::var("SDL_AUDIODRIVER").as_deref() == Ok("dummy") {
            cmd.env("SDL_AUDIODRIVER", "dummy");
        } else {
            local_output(&mut cmd)?;
        }
        cmd.env("SDL_VIDEODRIVER", "dummy");
        let mut child = cmd
            .args([
                "-hide_banner",
                "-v",
                "error",
                "-nostats",
                "-nodisp",
                "-autoexit",
                "-threads",
                "1",
                "-max_alloc",
                "33554432",
                "-protocol_whitelist",
                "pipe",
                "-format_whitelist",
                "s16le",
                "-codec_whitelist",
                "pcm_s16le",
                "-probesize",
                "128",
                "-analyzeduration",
                "0",
                "-f",
                "s16le",
                "-sample_rate",
                "24000",
                "-ch_layout",
                "mono",
                "-i",
                "pipe:0",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "Sound needs a local ffplay executable")?;
        let mut input = child.stdin.take().expect("piped audio");
        let (send, recv) = mpsc::sync_channel::<Vec<u8>>(super::FRAMES);
        let written = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&written);
        let writer = thread::spawn(move || {
            while let Ok(frame) = recv.recv() {
                if input.write_all(&frame).is_err() {
                    break;
                }
                count.fetch_add(frame.len(), Ordering::Relaxed);
            }
        });
        Ok(Self {
            child,
            send: Some(send),
            writer: Some(writer),
            written,
        })
    }
    pub fn write_frame(&mut self, pcm: &[u8]) -> Result<(), String> {
        if !matches!(self.child.try_wait(), Ok(None)) {
            return Err("Local audio output unavailable; video continues muted".into());
        }
        self.send
            .as_ref()
            .expect("open sink")
            .try_send(pcm.to_vec())
            .map_err(|_| {
                "Local audio output unavailable or not keeping up; video continues muted".into()
            })
    }
    pub fn written(&self) -> usize {
        self.written.load(Ordering::Relaxed)
    }
}
impl Drop for Sink {
    fn drop(&mut self) {
        // A dead device can block its writer on a full pipe. Terminate the reader before joining.
        let _ = self.child.kill();
        self.send.take();
        let _ = self.child.wait();
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
    }
}

#[cfg(target_os = "linux")]
fn local_output(cmd: &mut Command) -> Result<(), String> {
    use std::{os::unix::fs::FileTypeExt, path::PathBuf};
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| {
            p.is_absolute()
                && p.to_str().is_some_and(|s| {
                    s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"/_.-".contains(&b))
                })
        })
        .ok_or("No local PipeWire or PulseAudio socket; video continues muted")?;
    let socket = |name: &str| {
        dir.join(name)
            .metadata()
            .is_ok_and(|m| m.file_type().is_socket())
    };
    cmd.env("XDG_RUNTIME_DIR", &dir);
    if socket("pulse/native") {
        let mut target = std::ffi::OsString::from("unix:");
        target.push(dir.join("pulse/native"));
        cmd.env("SDL_AUDIODRIVER", "pulseaudio")
            .env("PULSE_SERVER", target);
    } else if socket("pipewire-0") {
        cmd.env("SDL_AUDIODRIVER", "pipewire")
            .env("PIPEWIRE_RUNTIME_DIR", &dir)
            .env("PIPEWIRE_REMOTE", "pipewire-0");
    } else {
        return Err("No local PipeWire or PulseAudio socket; video continues muted".into());
    }
    Ok(())
}
#[cfg(target_os = "macos")]
fn local_output(cmd: &mut Command) -> Result<(), String> {
    cmd.env("SDL_AUDIODRIVER", "coreaudio");
    Ok(())
}
#[cfg(target_os = "windows")]
fn local_output(cmd: &mut Command) -> Result<(), String> {
    cmd.env("SDL_AUDIODRIVER", "wasapi");
    Ok(())
}
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn local_output(_: &mut Command) -> Result<(), String> {
    Err("No supported local audio output; video continues muted".into())
}
