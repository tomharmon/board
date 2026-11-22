use anyhow::{Context, Result, ensure};
use std::{
    io::{Read, Seek, Write},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(super) fn png(bytes: &[u8]) -> Result<Vec<u8>> {
    const LIMIT: usize = 5 * 1024 * 1024;
    let mut input = tempfile::tempfile()?;
    input.write_all(bytes)?;
    input.rewind()?;
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
    cmd.env_clear();
    for key in ["PATH", "SystemRoot"] {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    let mut child = cmd
        .stdin(input)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("AVIF preview needs a local FFmpeg decoder")?;
    let output = child.stdout.take().expect("piped output");
    let reader = thread::spawn(move || {
        let mut png = Vec::new();
        output.take((LIMIT + 1) as u64).read_to_end(&mut png)?;
        Ok::<_, std::io::Error>(png)
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if start.elapsed() < Duration::from_secs(5) => {
                thread::sleep(Duration::from_millis(15))
            }
            _ => {
                let _ = child.kill();
                break child.wait();
            }
        }
    };
    let png = reader
        .join()
        .map_err(|_| anyhow::anyhow!("AVIF decoder failed"))??;
    ensure!(
        status?.success() && png.len() <= LIMIT,
        "Could not decode AVIF within the preview limits"
    );
    Ok(png)
}
