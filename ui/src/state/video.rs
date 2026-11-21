use rune::{Pixels, Tx};
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const W: u32 = 320;
const H: u32 = 180;
const FRAME: usize = (W * H * 4) as usize;
const MAX: usize = 12;
static DECODER: Mutex<()> = Mutex::new(());

/// An explicit six-second, silent sample. Only the selected board BLOB is piped to the local decoder.
pub struct Clip {
    pub frames: Vec<Pixels>,
    pub current: usize,
    pub playing: bool,
    pub error: Option<String>,
    pending: Option<mpsc::Receiver<Result<Vec<u8>, String>>>,
    cancel: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
    next: f64,
}
impl Drop for Clip {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Clip {
    pub fn decode(bytes: Vec<u8>) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&cancel);
        let (send, recv) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _ = send.send(decode(bytes, &stop));
        });
        Self {
            frames: vec![],
            current: 0,
            playing: false,
            error: None,
            pending: Some(recv),
            cancel,
            worker: Some(worker),
            next: 0.0,
        }
    }
    pub fn poll(&mut self, now: f64, tx: &mut Tx) -> bool {
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(Ok(bytes)) => {
                    self.frames = bytes
                        .as_chunks::<FRAME>()
                        .0
                        .iter()
                        .map(|frame| {
                            let p = Pixels::new();
                            p.set(W, H, frame.to_vec());
                            p
                        })
                        .collect();
                }
                Ok(Err(e)) => self.error = Some(e),
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.error = Some("Local decoder stopped".into())
                }
                Err(mpsc::TryRecvError::Empty) => {
                    tx.wake_after(Duration::from_millis(40));
                    return false;
                }
            }
            self.pending = None;
            return true;
        }
        if self.playing {
            tx.wake_after(Duration::from_millis(80));
            if self.next == 0.0 {
                self.next = now + 0.5;
            }
            if now >= self.next {
                if self.current + 1 < self.frames.len() {
                    self.current += 1;
                } else {
                    self.playing = false;
                }
                self.next = now + 0.5;
                return true;
            }
        }
        false
    }
    pub fn step(&mut self, delta: isize) {
        self.playing = false;
        self.current = self
            .current
            .saturating_add_signed(delta)
            .min(self.frames.len().saturating_sub(1));
    }
    pub fn play(&mut self) {
        if self.frames.is_empty() {
            return;
        }
        self.playing = !self.playing;
        if self.playing && self.current + 1 >= self.frames.len() {
            self.current = 0;
        }
        self.next = 0.0;
    }
}
fn decode(bytes: Vec<u8>, stop: &AtomicBool) -> Result<Vec<u8>, String> {
    let _guard = DECODER
        .try_lock()
        .map_err(|_| "A video preview is already loading".to_owned())?;
    let mut command = Command::new("ffmpeg");
    command.env_clear();
    for key in ["PATH", "SystemRoot"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    let mut p=command.args(["-v","error","-nostdin","-max_alloc","33554432","-threads","1","-filter_threads","1",
        "-protocol_whitelist","pipe","-format_whitelist","mov","-codec_whitelist","h264","-max_streams","8","-max_pixels","8000000",
        "-enable_drefs","0","-use_absolute_path","0","-f","mov","-i","pipe:0","-map","0:v:0","-an","-sn","-dn","-t","6",
        "-vf","fps=2,scale=320:180:force_original_aspect_ratio=decrease,pad=320:180:(ow-iw)/2:(oh-ih)/2,setsar=1",
        "-frames:v","12","-threads","1","-pix_fmt","rgba","-f","rawvideo","pipe:1"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
        .map_err(|_|"Video preview needs a local FFmpeg executable".to_owned())?;
    let mut input = p.stdin.take().expect("piped input");
    let output = p.stdout.take().expect("piped output");
    let writer = thread::spawn(move || {
        let _ = input.write_all(&bytes);
    });
    let reader = thread::spawn(move || {
        let mut rgba = Vec::new();
        output
            .take((FRAME * MAX + 1) as u64)
            .read_to_end(&mut rgba)
            .map(|_| rgba)
    });
    let start = Instant::now();
    let status = loop {
        if stop.load(Ordering::Relaxed) || start.elapsed() > Duration::from_secs(5) {
            let _ = p.kill();
            break p.wait();
        }
        match p.try_wait() {
            Ok(Some(s)) => break Ok(s),
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                let _ = p.kill();
                let _ = p.wait();
                break Err(e);
            }
        }
    };
    let _ = writer.join();
    let data = reader.join().ok().and_then(Result::ok);
    if status.is_ok_and(|s| s.success())
        && let Some(b) = data
        && !b.is_empty()
        && b.len() <= FRAME * MAX
        && b.len() % FRAME == 0
    {
        Ok(b)
    } else {
        Err("No bounded H.264 video preview available".into())
    }
}
