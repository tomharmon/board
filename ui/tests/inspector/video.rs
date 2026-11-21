use super::*;

#[cfg(target_os = "linux")]
fn players(pid: u32) -> Vec<u32> {
    let mut found = vec![];
    for t in fs::read_dir(format!("/proc/{pid}/task")).unwrap().flatten() {
        if let Ok(children) = fs::read_to_string(t.path().join("children")) {
            for id in children
                .split_whitespace()
                .filter_map(|id| id.parse::<u32>().ok())
            {
                if fs::read_to_string(format!("/proc/{id}/comm"))
                    .is_ok_and(|n| n.trim() == "ffplay")
                {
                    found.push(id);
                }
            }
        }
    }
    found.sort_unstable();
    found.dedup();
    found
}

#[cfg(target_os = "linux")]
#[test]
fn opt_in_sound_feeds_real_pcm_to_ffplay_and_reaps_it_on_pause_seek_and_revocation() {
    if !available("vp9")
        || !available("opus")
        || !Command::new("ffplay")
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    {
        return;
    }
    let f = Fixture::new();
    let db = Board::open(&f.db).unwrap();
    let actor = "reader/media";
    let grant = db
        .set_grant(
            "reader",
            None,
            actor,
            &board_local::access::Rules::parse("media=read").unwrap(),
        )
        .unwrap();
    let token = db
        .issue_credential("reader", actor, None, None)
        .unwrap()
        .token;
    let m = db
        .post(
            "media",
            "owner",
            "Sound check",
            &PostOptions {
                files: vec![(
                    "tone.webm".into(),
                    include_bytes!("../fixtures/tone.webm").to_vec(),
                )],
                ..Default::default()
            },
        )
        .unwrap();
    let open =
        json!({"t":0.1,"type":"tap","key":format!("preview-{}-{}",m.id,m.attachments[0].id)});
    let speaker = json!({"t":1.1,"type":"tap","key":"clip-sound"});
    let play = json!({"t":1.35,"type":"tap","key":"clip-play"});
    let mut c = f.command(json!([open, speaker, play]), "820x640", "3.55");
    let out = c
        .arg("--thread")
        .arg(m.id.to_string())
        .env("BOARD_TOKEN", &token)
        .env("SDL_AUDIODRIVER", "dummy")
        .arg("--realtime")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let tree = f.tree();
    tree.expect_key("clip-audio-active");
    let level = tree
        .0
        .iter()
        .find(|e| e["key"] == "clip-audio-active")
        .unwrap()["label"]
        .as_str()
        .unwrap();
    let peak = level
        .split_whitespace()
        .nth(2)
        .unwrap()
        .parse::<u8>()
        .unwrap();
    assert!(
        peak > 0,
        "expected the generated sine tone, not silence: {level}"
    );

    let mut cmd=f.command(json!([open,{"t":0.9,"type":"mark","name":"muted"},speaker,play,
        {"t":2.8,"type":"mark","name":"sounding"},{"t":3.1,"type":"tap","key":"clip-play"},{"t":3.4,"type":"mark","name":"paused"},
        {"t":3.8,"type":"tap","key":"clip-play"},{"t":5.0,"type":"mark","name":"resumed"},
        {"t":5.35,"type":"tap","key":"clip-seek"},{"t":5.65,"type":"mark","name":"sought"},
        {"t":6.05,"type":"tap","key":"clip-play"},{"t":7.45,"type":"mark","name":"revoke"},{"t":9.0,"type":"mark","name":"redacted"}
    ]),"820x640","9.35");
    let mut child = cmd
        .arg("--thread")
        .arg(m.id.to_string())
        .env("BOARD_TOKEN", &token)
        .env("SDL_AUDIODRIVER", "dummy")
        .arg("--realtime")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut seen = Vec::new();
    let mut counts = Vec::new();
    let mut cmdline = vec![];
    for line in BufReader::new(child.stdout.take().unwrap())
        .lines()
        .map_while(Result::ok)
    {
        let Some(name) = line
            .strip_prefix("rune-mark ")
            .and_then(|s| s.split_whitespace().next())
        else {
            continue;
        };
        let active = players(child.id());
        if name == "sounding"
            && let Some(pid) = active.first()
        {
            cmdline = fs::read(format!("/proc/{pid}/cmdline")).unwrap();
        }
        if name == "revoke" {
            db.set_grant(
                "reader",
                Some(grant.id),
                actor,
                &board_local::access::Rules::parse("elsewhere=read").unwrap(),
            )
            .unwrap();
        }
        seen.push(name.to_owned());
        counts.push(active.len());
    }
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        seen,
        [
            "muted", "sounding", "paused", "resumed", "sought", "revoke", "redacted"
        ]
    );
    assert_eq!(
        counts,
        [0, 1, 0, 1, 0, 1, 0],
        "a stale ffplay must never continue under the other state"
    );
    let args = cmdline
        .split(|v| *v == 0)
        .filter_map(|s| std::str::from_utf8(s).ok())
        .collect::<Vec<_>>();
    assert!(
        args.contains(&"-nodisp")
            && args.windows(2).any(|a| a == ["-f", "s16le"])
            && args.windows(2).any(|a| a == ["-i", "pipe:0"])
            && !args.iter().any(|s| s.contains(".webm"))
    );
    let tree = f.tree();
    assert!(!tree.has_key("clip-frame") && !tree.has_key("clip-audio-active"));
}

#[cfg(target_os = "linux")]
#[test]
fn video_survives_missing_local_audio_device_or_player() {
    if !available("vp9") || !available("opus") {
        return;
    }
    let f = Fixture::new();
    let db = Board::open(&f.db).unwrap();
    let m = db
        .post(
            "media",
            "owner",
            "Audio fallback",
            &PostOptions {
                files: vec![(
                    "tone.webm".into(),
                    include_bytes!("../fixtures/tone.webm").to_vec(),
                )],
                ..Default::default()
            },
        )
        .unwrap();
    let path = f.dir.path().join("tools");
    fs::create_dir(&path).unwrap();
    for name in ["ffprobe", "ffmpeg"] {
        let exe = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|dir| dir.join(name))
            .find(|p| p.is_file())
            .unwrap();
        std::os::unix::fs::symlink(exe, path.join(name)).unwrap();
    }
    for (missing, reason) in [
        (
            false,
            "No local PipeWire or PulseAudio socket; video continues muted",
        ),
        (true, "Sound needs a local ffplay executable"),
    ] {
        let mut cmd = f.command(
            json!([{"t":0.1,"type":"tap","key":format!("preview-{}-{}",m.id,m.attachments[0].id)},
            {"t":1.05,"type":"tap","key":"clip-sound"},{"t":1.3,"type":"tap","key":"clip-play"}]),
            "820x640",
            "2.9",
        );
        if missing {
            cmd.env("SDL_AUDIODRIVER", "dummy").env("PATH", &path);
        } else {
            cmd.env_remove("SDL_AUDIODRIVER")
                .env("XDG_RUNTIME_DIR", f.dir.path());
        }
        let out = cmd
            .arg("--thread")
            .arg(m.id.to_string())
            .arg("--realtime")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let t = f.tree();
        t.expect_text(reason);
        t.expect_key("clip-frame");
        assert!(!t.has_key("clip-audio-active"));
    }
}

pub(super) fn available(codec: &str) -> bool {
    let has_fd = |exe: &str| {
        Command::new(exe)
            .args(["-hide_banner", "-protocols"])
            .output()
            .is_ok_and(|o| {
                o.status.success()
                    && String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .any(|line| line.trim() == "fd")
            })
    };
    has_fd("ffmpeg")
        && has_fd("ffprobe")
        && Command::new("ffmpeg")
            .args(["-hide_banner", "-decoders"])
            .output()
            .is_ok_and(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .any(|line| line.split_whitespace().nth(1) == Some(codec))
            })
}

#[cfg(target_os = "linux")]
#[test]
fn mp4_aac_sound_is_opt_in_and_reaches_the_local_dummy_sink() {
    if !available("aac")
        || !available("h264")
        || !Command::new("ffplay")
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    {
        return;
    }
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let m = b
        .post(
            "media",
            "owner",
            "AAC tone",
            &PostOptions {
                files: vec![(
                    "audio.mp4".into(),
                    include_bytes!("../fixtures/tone-aac.mp4").to_vec(),
                )],
                ..Default::default()
            },
        )
        .unwrap();
    let out = f
        .command(
            json!([{"t":0.1,"type":"tap","key":format!("preview-{}-{}",m.id,m.attachments[0].id)},
        {"t":1.05,"type":"tap","key":"clip-sound"},{"t":1.3,"type":"tap","key":"clip-play"}]),
            "820x640",
            "2.8",
        )
        .arg("--thread")
        .arg(m.id.to_string())
        .arg("--realtime")
        .env("SDL_AUDIODRIVER", "dummy")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let t = f.tree();
    t.expect_key("clip-audio-active");
}

#[test]
fn webm_play_pause_and_end_seek_decode_real_frames_outside_the_first_segment() {
    if !available("vp9") {
        return;
    }
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let m = b
        .post(
            "video",
            "owner",
            "Review both scenes",
            &PostOptions {
                files: vec![(
                    "timeline.webm".into(),
                    include_bytes!("../fixtures/timeline.webm").to_vec(),
                )],
                ..Default::default()
            },
        )
        .unwrap();
    let open =
        json!({"t":0.1,"type":"tap","key":format!("preview-{}-{}",m.id,m.attachments[0].id)});
    let play = json!({"t":1.15,"type":"tap","key":"clip-play"});
    let mut c = f.command(
        json!([open,play,{"t":3.35,"type":"tap","key":"clip-play"}]),
        "820x640",
        "4.65",
    );
    let out = c
        .arg("--thread")
        .arg(m.id.to_string())
        .arg("--realtime")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let t = f.tree();
    t.expect_key("clip-frame");
    let clock = t.0.iter().find(|e| e["key"] == "clip-position").unwrap()["text"]
        .as_str()
        .unwrap();
    let frame = clock
        .split_once("Frame ")
        .unwrap()
        .1
        .split_whitespace()
        .next()
        .unwrap()
        .parse::<usize>()
        .unwrap();
    assert!(
        (13..40).contains(&frame),
        "Play should cross a segment, then pause: {clock}"
    );
    assert!(
        t.0.iter()
            .any(|e| e["key"] == "clip-play" && e["label"] == "Play video")
    );

    let shot = f.dir.path().join("late-webm.png");
    // Rune snapshots at a scripted time, not on element readiness. Leave a decoder
    // deadline (5 seconds) after the last seek for the actual paused frame to paint.
    let mut c = f.command(
        json!([open,{"t":1.15,"type":"tap","key":"clip-seek"},
        {"t":1.55,"type":"key","name":"End"},{"t":2.15,"type":"tap","key":"clip-prev"}]),
        "820x640",
        "8.0",
    );
    let out = c
        .arg("--thread")
        .arg(m.id.to_string())
        .arg("--realtime")
        .arg("--snap")
        .arg(&shot)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let t = f.tree();
    t.expect_key("clip-frame");
    t.expect_text("Silent video   0:09 / 0:10   Frame 119 of 120");
    let frame = image::open(shot).unwrap().to_rgb8();
    assert!(
        frame
            .pixels()
            .filter(|p| p[0] > 220 && p[1] < 40 && p[2] > 220)
            .count()
            > 7000,
        "the second scene must actually reach the framebuffer"
    );
}

#[test]
fn vp8_viewer_releases_its_frames_when_a_live_credential_loses_the_channel() {
    if !available("vp8") {
        return;
    }
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let m = b
        .post(
            "media",
            "owner",
            "Local video",
            &PostOptions {
                files: vec![(
                    "green.webm".into(),
                    include_bytes!("../fixtures/green-vp8.webm").to_vec(),
                )],
                ..Default::default()
            },
        )
        .unwrap();
    let actor = "reader/media";
    let grant = b
        .set_grant(
            "reader",
            None,
            actor,
            &board_local::access::Rules::parse("media=read").unwrap(),
        )
        .unwrap();
    let token = b
        .issue_credential("reader", actor, None, None)
        .unwrap()
        .token;
    let open =
        json!({"t":0.1,"type":"tap","key":format!("preview-{}-{}",m.id,m.attachments[0].id)});
    // Prove that pixels are present before testing redaction, even on a busy software GPU.
    let before = f
        .command(json!([open]), "820x640", "6.5")
        .arg("--thread")
        .arg(m.id.to_string())
        .env("BOARD_TOKEN", &token)
        .arg("--realtime")
        .output()
        .unwrap();
    assert!(
        before.status.success(),
        "{}",
        String::from_utf8_lossy(&before.stderr)
    );
    f.tree().expect_key("clip-frame");
    let mut c=f.command(json!([open,{"t":1.1,"type":"tap","key":"clip-play"},{"t":1.3,"type":"mark","name":"video-open"}]),"820x640","3.2");
    c.arg("--thread")
        .arg(m.id.to_string())
        .env("BOARD_TOKEN", token);
    let t = f.at_mark(&mut c, "video-open", || {
        b.set_grant(
            "reader",
            Some(grant.id),
            actor,
            &board_local::access::Rules::parse("elsewhere=read").unwrap(),
        )
        .unwrap();
    });
    assert!(
        !t.has_key("clip-frame") && !t.has_key("clip-seek") && !t.has_key("attachment-preview")
    );
    assert!(!t.0.iter().any(|e| e.to_string().contains("green.webm")));
}
