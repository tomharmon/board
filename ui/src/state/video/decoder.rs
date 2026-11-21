use super::{BYTES, FPS, FRAMES, Format};
use serde::Deserialize;
use std::{
    fs::File,
    io::{Read, Seek},
    process::{Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

static DECODER: Mutex<()> = Mutex::new(());

// An EBML magic number also identifies arbitrary Matroska files. Read the bounded
// EBML Header and require its real DocType element, not a stray `webm` in a tag.
pub(super) fn webm_header(data: &[u8]) -> bool {
    fn vint(bytes: &[u8], id: bool) -> Option<(u64, usize)> {
        let first = *bytes.first()?;
        if first == 0 {
            return None;
        }
        let n = first.leading_zeros() as usize + 1;
        let head = if id {
            first
        } else {
            first & 0xffu8.checked_shr(n as u32).unwrap_or(0)
        };
        let value = bytes
            .get(1..n)?
            .iter()
            .fold(u64::from(head), |a, b| (a << 8) | u64::from(*b));
        Some((value, n))
    }
    let Some(head) = data.strip_prefix(&[0x1a, 0x45, 0xdf, 0xa3]) else {
        return false;
    };
    let Some((len, n)) = vint(head, false) else {
        return false;
    };
    if len > 4096 {
        return false;
    }
    let Some(mut rest) = head.get(n..n + len as usize) else {
        return false;
    };
    while !rest.is_empty() {
        let Some((id, i)) = vint(rest, true) else {
            return false;
        };
        let Some((size, j)) = vint(&rest[i..], false) else {
            return false;
        };
        if size > 4096 {
            return false;
        }
        let end = i + j + size as usize;
        let Some(value) = rest.get(i + j..end) else {
            return false;
        };
        if id == 0x4282 {
            return value == b"webm";
        }
        rest = &rest[end..];
    }
    false
}

#[derive(Clone, Copy)]
enum Input {
    Probe,
    Video,
    Audio,
}
fn input_args(cmd: &mut Command, format: Format, input: Input) {
    let (demux, whitelist, codecs) = match format {
        Format::Mov => (
            "mov",
            "mov",
            match input {
                Input::Probe => "h264,aac,mp3,mp3float",
                Input::Video => "h264",
                Input::Audio => "aac,mp3,mp3float",
            },
        ),
        Format::Webm => (
            "webm",
            "matroska,webm",
            match input {
                Input::Probe => "vp8,vp9,opus,vorbis",
                Input::Video => "vp8,vp9",
                Input::Audio => "opus,vorbis",
            },
        ),
    };
    cmd.args([
        "-max_alloc",
        "33554432",
        "-threads",
        "1",
        "-probesize",
        "4194304",
        "-analyzeduration",
        "2000000",
        "-protocol_whitelist",
        "fd",
        "-format_whitelist",
        whitelist,
        "-codec_whitelist",
        codecs,
        "-max_streams",
        "8",
        "-max_pixels",
        "8000000",
    ]);
    if matches!(format, Format::Mov) {
        cmd.args(["-enable_drefs", "0", "-use_absolute_path", "0"]);
    }
    cmd.args(["-f", demux]);
}
pub(super) fn run(
    mut command: Command,
    file: &File,
    limit: usize,
    stop: &AtomicBool,
) -> Result<Vec<u8>, String> {
    let mut input = file
        .try_clone()
        .map_err(|_| "Local video handle unavailable")?;
    input
        .rewind()
        .map_err(|_| "Local video handle unavailable")?;
    command.env_clear();
    for key in ["PATH", "SystemRoot"] {
        if let Some(v) = std::env::var_os(key) {
            command.env(key, v);
        }
    }
    let mut child = command
        .stdin(Stdio::from(input))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "Video playback needs local ffmpeg and ffprobe executables")?;
    let output = child.stdout.take().expect("piped output");
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        output
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let start = Instant::now();
    let status = loop {
        if stop.load(Ordering::Relaxed) || start.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            break child.wait();
        }
        match child.try_wait() {
            Ok(Some(s)) => break Ok(s),
            Ok(None) => thread::sleep(Duration::from_millis(15)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(e);
            }
        }
    };
    let data = reader.join().ok().and_then(Result::ok);
    if status.is_ok_and(|s| s.success())
        && let Some(data) = data
        && data.len() <= limit
    {
        Ok(data)
    } else {
        Err(
            "Video unavailable: unsupported stream, decoder limit or missing FFmpeg fd protocol"
                .into(),
        )
    }
}
#[derive(Deserialize)]
struct Probe {
    streams: Vec<Stream>,
    format: DurationInfo,
}
#[derive(Deserialize)]
struct DurationInfo {
    duration: Option<String>,
}
#[derive(Deserialize)]
struct Stream {
    codec_name: String,
    width: u32,
    height: u32,
    duration: Option<String>,
}
fn probe(file: &File, format: Format, stop: &AtomicBool) -> Result<usize, String> {
    let mut cmd = Command::new("ffprobe");
    cmd.args(["-v", "error"]);
    input_args(&mut cmd, format, Input::Probe);
    cmd.args([
        "-select_streams",
        "V:0",
        "-show_entries",
        "stream=codec_name,width,height,duration:format=duration",
        "-of",
        "json",
        "-i",
        "fd:",
    ]);
    let p: Probe = serde_json::from_slice(&run(cmd, file, 16384, stop)?)
        .map_err(|_| "No supported local video stream")?;
    let s = p.streams.first().ok_or("No supported local video stream")?;
    if !matches!(
        (format, s.codec_name.as_str()),
        (Format::Mov, "h264") | (Format::Webm, "vp8" | "vp9")
    ) || s.width == 0
        || s.height == 0
        || s.width.max(s.height) > 4096
        || u64::from(s.width) * u64::from(s.height) > 8_000_000
    {
        return Err("Video codec or dimensions are outside the preview limits".into());
    }
    let seconds = [s.duration.as_deref(), p.format.duration.as_deref()]
        .into_iter()
        .flatten()
        .filter_map(|s| s.parse::<f64>().ok())
        .find(|s| s.is_finite() && *s > 0. && *s <= 86_400.)
        .ok_or("Video needs a finite duration of at most 24 hours")?;
    Ok((seconds * FPS as f64).ceil() as usize)
}
pub(super) struct Segment {
    pub total: usize,
    pub rgba: Vec<u8>,
    pub pcm: Option<Result<Vec<u8>, String>>,
}
pub(super) fn segment(
    file: &File,
    format: Format,
    first: usize,
    total: usize,
    sound: Option<bool>,
    stop: &AtomicBool,
) -> Result<Segment, String> {
    let _guard = DECODER
        .try_lock()
        .map_err(|_| "Another video segment is loading")?;
    let total = if total == 0 {
        probe(file, format, stop)?
    } else {
        total
    };
    if stop.load(Ordering::Relaxed) {
        return Err("Video seek canceled".into());
    }
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-v",
        "error",
        "-nostdin",
        "-filter_threads",
        "1",
        "-hwaccel",
        "none",
    ]);
    input_args(&mut cmd, format, Input::Video);
    cmd.args(["-ss",&format!("{:.6}",first as f64/FPS as f64),"-i","fd:","-map","0:V:0","-an","-sn","-dn","-t","1",
        "-vf","fps=12,scale=320:180:force_original_aspect_ratio=decrease,pad=320:180:(ow-iw)/2:(oh-ih)/2,setsar=1","-frames:v","12","-threads","1","-pix_fmt","rgba","-f","rawvideo","pipe:1"]);
    let data = run(cmd, file, BYTES * FRAMES, stop)?;
    if data.is_empty() || !data.len().is_multiple_of(BYTES) {
        return Err("No video frame at this position".into());
    }
    // Never run the two demuxers concurrently: their inherited, anonymous handles share an offset.
    let pcm = sound.map(|check| {
        if stop.load(Ordering::Relaxed) {
            return Err("Audio request canceled".into());
        }
        if check {
            #[derive(Deserialize)]
            struct Audio {
                codec_name: String,
                channels: u16,
                sample_rate: String,
            }
            #[derive(Deserialize)]
            struct Tracks {
                streams: Vec<Audio>,
            }
            let mut info = Command::new("ffprobe");
            info.args(["-v", "error"]);
            input_args(&mut info, format, Input::Probe);
            info.args([
                "-select_streams",
                "a:0",
                "-show_entries",
                "stream=codec_name,channels,sample_rate",
                "-of",
                "json",
                "-i",
                "fd:",
            ]);
            let checked = run(info, file, 8192, stop)
                .ok()
                .and_then(|json| serde_json::from_slice::<Tracks>(&json).ok());
            let allowed = checked
                .as_ref()
                .and_then(|tracks| tracks.streams.first())
                .is_some_and(|a| {
                    matches!(
                        (format, a.codec_name.as_str()),
                        (Format::Mov, "aac" | "mp3") | (Format::Webm, "opus" | "vorbis")
                    ) && (1..=8).contains(&a.channels)
                        && a.sample_rate
                            .parse::<u32>()
                            .is_ok_and(|r| (8_000..=192_000).contains(&r))
                });
            if !allowed {
                return Err("No supported audio track; video continues muted".into());
            }
        }
        let mut audio = Command::new("ffmpeg");
        audio.args(["-v", "error", "-nostdin", "-filter_threads", "1"]);
        input_args(&mut audio, format, Input::Audio);
        audio.args([
            "-ss",
            &format!("{:.6}", first as f64 / FPS as f64),
            "-i",
            "fd:",
            "-map",
            "0:a:0",
            "-vn",
            "-sn",
            "-dn",
            "-t",
            "1",
            "-ac",
            "1",
            "-ar",
            "24000",
            "-threads",
            "1",
            "-c:a",
            "pcm_s16le",
            "-f",
            "s16le",
            "pipe:1",
        ]);
        run(audio, file, super::sound::RATE * 2, stop)
            .ok()
            .filter(|v| !v.is_empty() && v.len().is_multiple_of(2))
            .ok_or_else(|| "No supported audio at this position; video continues muted".into())
    });
    Ok(Segment {
        total,
        rgba: data,
        pcm,
    })
}
