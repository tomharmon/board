//! Exercise the shipped native binary, not a second implementation of the screens.
use std::{
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Condvar, Mutex},
};

use board_local::{
    Board, Filter, PostOptions,
    automation::{TaskAction, TaskTarget, change_task, create_task},
};
use serde_json::{Value, json};
use tempfile::TempDir;

#[path = "inspector/composing.rs"]
mod composing;
#[path = "inspector/picker.rs"]
mod picker;
#[path = "inspector/presentation.rs"]
mod presentation;
#[path = "inspector/reading.rs"]
mod reading;
#[path = "inspector/source_settings.rs"]
mod source_settings;
#[path = "inspector/task_conversation.rs"]
mod task_conversation;
#[path = "inspector/video.rs"]
mod video;
#[path = "inspector/work_views.rs"]
mod work_views;
#[path = "inspector/zoom.rs"]
mod zoom;

// Each shipped binary creates a whole graphics device, including a software
// renderer's thread pool on CPU-only builders. Bound those devices independently
// of libtest's CPU-based thread count.
static RENDERERS: (Mutex<u8>, Condvar) = (Mutex::new(3), Condvar::new());
const DESKTOP: &str = "1200x800";
struct RendererSlot;
impl RendererSlot {
    fn take() -> Self {
        let (count, available) = &RENDERERS;
        let mut n = available
            .wait_while(count.lock().unwrap(), |n| *n == 0)
            .unwrap();
        *n -= 1;
        Self
    }
}
impl Drop for RendererSlot {
    fn drop(&mut self) {
        let (count, available) = &RENDERERS;
        *count.lock().unwrap() += 1;
        available.notify_one();
    }
}

struct Fixture {
    dir: TempDir,
    db: PathBuf,
    _renderer: RendererSlot,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("board.sqlite3");
        Self {
            dir,
            db,
            _renderer: RendererSlot::take(),
        }
    }

    fn command(&self, steps: Value, size: &str, end: &str) -> Command {
        let script = self.dir.path().join("steps.json");
        fs::write(&script, serde_json::to_vec(&steps).unwrap()).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_board-ui"));
        // On displayless Linux builders SwiftShader may be installed outside the
        // loader's search path. This selects a real local ICD, never a mock render.
        #[cfg(target_os = "linux")]
        {
            let swift = Path::new("/usr/lib/chromium/vk_swiftshader_icd.json");
            if !Path::new("/dev/dri").exists()
                && swift.is_file()
                && std::env::var_os("VK_DRIVER_FILES").is_none()
                && std::env::var_os("VK_ICD_FILENAMES").is_none()
            {
                cmd.env("VK_DRIVER_FILES", swift);
            }
        }
        cmd.env("BOARD_DB", &self.db)
            .env("BOARD_USER", "reader")
            .env("BOARD_UI_THEME", "light")
            .env_remove("BOARD_PROFILE")
            .env_remove("BOARD_AVATAR")
            .env_remove("BOARD_TOKEN")
            .env("RUNE_DIRS_ROOT", self.dir.path().join("rune"))
            .arg("--script")
            .arg(script)
            .arg("--tree")
            .arg(self.dir.path().join("tree.json"))
            .args(["--fps", "12", "--size", size, "--at", end, "--scale", "1"]);
        cmd
    }

    fn render(&self, steps: Value, size: &str, end: &str) -> Tree {
        let output = self.command(steps, size, end).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        self.tree()
    }

    fn snapshot(
        &self,
        steps: Value,
        size: (u32, u32),
        theme: &str,
        name: &str,
    ) -> (Tree, image::RgbaImage) {
        let path = self.dir.path().join(format!("{name}.png"));
        let output = self
            .command(steps, &format!("{}x{}", size.0, size.1), "1.2")
            .env("BOARD_UI_THEME", theme)
            .arg("--snap")
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let tree = self.tree();
        let frame = read_frame(&path, size);
        // Only the synthetic layout fixtures are eligible for optional capture.
        if let Some(dir) = std::env::var_os("BOARD_UI_TEST_ARTIFACTS") {
            let dir = PathBuf::from(dir);
            fs::create_dir_all(&dir).unwrap();
            fs::copy(&path, dir.join(format!("{name}.png"))).unwrap();
            fs::copy(
                self.dir.path().join("tree.json"),
                dir.join(format!("{name}.tree.json")),
            )
            .unwrap();
        }
        (tree, frame)
    }

    fn at_mark(&self, cmd: &mut Command, mark: &str, write: impl FnOnce()) -> Tree {
        let mut ui = cmd
            .arg("--realtime")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Rune prints this mark in wall time once the scripted editor is on screen.
        let mark = format!("rune-mark {mark} ");
        let saw_mark = BufReader::new(ui.stdout.take().unwrap())
            .lines()
            .map_while(Result::ok)
            .any(|line| line.starts_with(&mark));
        if saw_mark {
            write();
        }
        let output = ui.wait_with_output().unwrap();
        assert!(
            saw_mark && output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        self.tree()
    }

    fn tree(&self) -> Tree {
        let snapshot: Value =
            serde_json::from_reader(File::open(self.dir.path().join("tree.json")).unwrap())
                .unwrap();
        assert_eq!(snapshot["warnings"], json!([]));
        Tree(snapshot["elements"].as_array().unwrap().clone())
    }
}

struct Tree(Vec<Value>);

#[derive(Copy, Clone, Debug)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}
impl Rect {
    fn edge(self) -> f64 {
        self.x + self.w
    }
    fn bottom(self) -> f64 {
        self.y + self.h
    }
}

impl Tree {
    fn has_key(&self, key: &str) -> bool {
        self.0.iter().any(|e| e["key"] == key)
    }
    fn bounds(&self, key: &str) -> Rect {
        self.expect_key(key);
        let el = self.0.iter().find(|e| e["key"] == key).unwrap();
        let rect: &[Value; 4] = el["rect"]
            .as_array()
            .unwrap()
            .as_slice()
            .try_into()
            .unwrap();
        let [x, y, w, h] = rect.each_ref().map(|v| v.as_f64().unwrap());
        Rect { x, y, w, h }
    }
    fn expect_key(&self, key: &str) {
        assert!(
            self.0.iter().any(|e| e["key"] == key),
            "no element with key {key}; visible text: {:?}",
            self.0
                .iter()
                .filter_map(|e| e["text"].as_str())
                .collect::<Vec<_>>()
        );
    }

    fn expect_text(&self, text: &str) {
        let texts: Vec<_> = self.0.iter().filter_map(|e| e["text"].as_str()).collect();
        assert!(texts.contains(&text), "no {text:?} in {texts:?}");
    }
}

fn read_frame(path: &Path, size: (u32, u32)) -> image::RgbaImage {
    let mut reader = png::Decoder::new(BufReader::new(File::open(path).unwrap()))
        .read_info()
        .unwrap();
    let mut rgba = vec![0; reader.output_buffer_size().unwrap()];
    let frame = reader.next_frame(&mut rgba).unwrap();
    assert_eq!((frame.width, frame.height), size);
    assert_eq!(frame.color_type, png::ColorType::Rgba);
    rgba.truncate(frame.buffer_size());
    image::RgbaImage::from_raw(frame.width, frame.height, rgba).unwrap()
}

fn check_surface(pixels: &image::RgbaImage, rect: Rect, dark: bool) {
    let (mut area, mut neutral, mut ink, mut paper) = (0u64, 0u64, 0u64, 0u64);
    for y in
        rect.y.max(0.0).ceil() as u32..rect.bottom().min(f64::from(pixels.height())).floor() as u32
    {
        for x in
            rect.x.max(0.0).ceil() as u32..rect.edge().min(f64::from(pixels.width())).floor() as u32
        {
            let [r, g, b, a] = pixels[(x, y)].0;
            let lo = r.min(g).min(b);
            let hi = r.max(g).max(b);
            area += 1;
            neutral += u64::from(a == 255 && hi - lo <= 16);
            paper += u64::from(a == 255 && if dark { hi <= 55 } else { lo >= 225 });
            ink += u64::from(a == 255 && if dark { lo >= 175 } else { hi <= 110 });
        }
    }
    assert!(
        area > 0 && paper > area / 2 && neutral * 100 > area * 98 && ink > 180,
        "unpainted or wrong theme in {rect:?} (dark={dark}): {ink} foreground, {paper} background, {neutral}/{area} neutral"
    );
}

fn check_pixels(path: &Path, size: (u32, u32)) {
    check_surface(
        &read_frame(path, size),
        Rect {
            x: 0.0,
            y: 0.0,
            w: f64::from(size.0),
            h: f64::from(size.1),
        },
        false,
    );
}

fn layout_fixture(f: &Fixture) -> (i64, i64, i64) {
    let b = Board::open(&f.db).unwrap();
    let first = b
        .post(
            "desk",
            "reader",
            "Compare lock outcomes",
            &PostOptions::default(),
        )
        .unwrap();
    let second = b
        .post(
            "desk",
            "writer",
            "Review backup record",
            &PostOptions::default(),
        )
        .unwrap();
    let reply = b
        .reply(
            first.id,
            "writer",
            "Pinned reader saw the same snapshot",
            &Default::default(),
        )
        .unwrap();
    (first.id, second.id, reply.id)
}

fn close_to(actual: f64, expected: f64, tolerance: f64, detail: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{detail}: {actual}, expected {expected} +/- {tolerance}"
    );
}
fn panel_header(tree: &Tree, key: &str, pane: Rect) {
    let bar = tree.bounds(key);
    close_to(bar.x, pane.x, 2.0, key);
    close_to(bar.y, pane.y, 2.0, key);
    close_to(bar.w, pane.w, 7.0, key);
    close_to(bar.h, 46.0, 1.0, key);
}

#[test]
fn desktop_shell_keeps_the_activity_feed_beside_an_independently_switchable_thread() {
    let f = Fixture::new();
    let (first, second, reply) = layout_fixture(&f);
    let pick = |t: f64, id: i64| json!({"t":t,"type":"tap","key":format!("thread-{id}")});
    let (t, frame) = f.snapshot(
        json!([pick(0.2, first)]),
        (1200, 800),
        "light",
        "desktop-thread-light",
    );
    let rail = t.bounds("channels");
    let feed = t.bounds("feed-pane");
    let thread = t.bounds("thread-pane");
    assert!(
        (200.0..=240.0).contains(&rail.w),
        "unexpected sidebar: {rail:?}"
    );
    assert!(
        feed.w >= 225.0 && thread.w >= 250.0,
        "unusable panes: {feed:?}, {thread:?}"
    );
    close_to(rail.w, 224.0, 2.0, "default desktop sidebar width");
    close_to(thread.w, 440.0, 2.0, "default desktop thread width");
    close_to(rail.x, 0.0, 1.0, "sidebar origin");
    close_to(rail.edge(), feed.x, 8.0, "sidebar/feed boundary");
    close_to(feed.edge(), thread.x, 8.0, "feed/thread boundary");
    close_to(thread.edge(), 1200.0, 2.0, "right edge");
    for (header, pane) in [
        ("sidebar-header", rail),
        ("feed-header", feed),
        ("thread-header", thread),
    ] {
        close_to(pane.y, 0.0, 2.0, header);
        close_to(pane.bottom(), 800.0, 2.0, header);
        panel_header(&t, header, pane);
        check_surface(&frame, pane, false);
    }
    t.expect_key("sidebar-resize");
    t.expect_key("thread-resize");
    t.expect_key("feed-compose");
    assert_eq!(
        t.0.iter().filter(|e| e["key"] == "message-body").count(),
        1,
        "two panes must not compete for the active message editor"
    );
    t.expect_key(&format!("activity-reply-{reply}"));
    t.expect_key(&format!("message-{reply}"));
    let oldest = t.bounds(&format!("thread-{first}"));
    let feed_item = t.bounds(&format!("thread-{second}"));
    let newest = t.bounds(&format!("activity-reply-{reply}"));
    assert!(
        oldest.bottom() <= feed_item.y + 1.0 && feed_item.bottom() <= newest.y + 1.0,
        "all activity must show distinct non-overlapping roots and replies in chronological order: {oldest:?}, {feed_item:?}, {newest:?}"
    );
    let message = t.bounds(&format!("message-{first}"));
    assert!(feed_item.x >= feed.x - 1.0 && feed_item.edge() <= feed.edge() + 1.0);
    assert!(message.x >= thread.x - 1.0 && message.edge() <= thread.edge() + 1.0);

    let (next, dark) = f.snapshot(
        json!([pick(0.2, first), pick(0.6, second)]),
        (1200, 800),
        "dark",
        "desktop-thread-dark",
    );
    next.expect_key(&format!("thread-{first}"));
    next.expect_key(&format!("message-{second}"));
    assert!(
        !next.has_key(&format!("message-{reply}")),
        "switched pane still shows old conversation"
    );
    for key in ["channels", "feed-pane", "thread-pane"] {
        check_surface(&dark, next.bounds(key), true);
    }
    let back = f.render(
        json!([pick(0.2,first),pick(0.5,second),{"t":0.8,"type":"tap","key":"back-to-feed"}]),
        "1200x800",
        "1.8",
    );
    assert!(!back.has_key("thread-pane"));
    close_to(
        back.bounds("feed-pane").edge(),
        1200.0,
        2.0,
        "fully expanded feed",
    );
    back.expect_key(&format!("activity-reply-{reply}"));
    let resized = f.render(
        json!([pick(0.2,first),
        {"t":0.4,"type":"tap","key":"sidebar-resize"},{"t":0.6,"type":"key","name":"ArrowRight"},
        {"t":0.85,"type":"drag","key":"thread-resize","dx":-36,"dur":0.25}]),
        "1200x800",
        "1.35",
    );
    assert!(
        resized.bounds("channels").w > rail.w + 1.0,
        "focused divider did not resize with the keyboard"
    );
    assert!(
        resized.bounds("thread-pane").w > thread.w + 15.0,
        "drag did not expand the thread pane"
    );
    resized.expect_key(&format!("message-{first}"));
}

#[test]
fn compact_shell_opens_navigation_on_demand_and_returns_from_a_full_width_thread() {
    let f = Fixture::new();
    let (first, _, reply) = layout_fixture(&f);
    let (initial, frame) = f.snapshot(json!([]), (680, 720), "light", "compact-activity");
    initial.expect_key("open-nav");
    initial.expect_key(&format!("activity-reply-{reply}"));
    let feed = initial.bounds("feed-pane");
    close_to(feed.x, 0.0, 2.0, "compact feed origin");
    close_to(feed.w, 680.0, 2.0, "compact feed width");
    assert!(!initial.has_key("channels") && !initial.has_key("thread-pane"));
    panel_header(&initial, "feed-header", feed);
    check_surface(&frame, feed, false);

    let menu = json!({"t":0.18,"type":"tap","key":"open-nav"});
    let channel = json!({"t":0.45,"type":"tap","key":"nav-desk"});
    let open = json!({"t":0.65,"type":"tap","key":format!("thread-{first}")});
    let nav = f.render(json!([menu]), "680x720", "0.5");
    nav.expect_key("channels");
    nav.expect_key("close-nav");
    nav.expect_key("nav-scrim");
    close_to(nav.bounds("channels").w, 280.0, 2.0, "compact drawer width");
    nav.expect_key("nav-desk");
    let dismissed = f.render(
        json!([menu,{"t":0.4,"type":"key","name":"Escape"}]),
        "680x720",
        "0.75",
    );
    dismissed.expect_key("feed-pane");
    dismissed.expect_key("open-nav");
    assert!(
        !dismissed.has_key("channels"),
        "Escape didn't close the navigation drawer"
    );
    let (thread, frame) = f.snapshot(
        json!([menu, channel, open]),
        (680, 720),
        "light",
        "compact-thread",
    );
    let pane = thread.bounds("thread-pane");
    close_to(pane.x, 0.0, 2.0, "compact thread origin");
    close_to(pane.w, 680.0, 2.0, "compact thread width");
    panel_header(&thread, "thread-header", pane);
    thread.expect_key(&format!("message-{reply}"));
    assert!(
        !thread.has_key("channels")
            && !thread.has_key("feed-pane")
            && !thread.has_key("thread-resize")
    );
    check_surface(&frame, pane, false);
    let closed = f.render(
        json!([menu,channel,open,{"t":0.85,"type":"tap","key":"back-to-feed"}]),
        "680x720",
        "1.85",
    );
    closed.expect_key(&format!("thread-{first}"));
    closed.expect_key("open-nav");
    closed.expect_key("feed-pane");
    assert!(
        !closed.has_key("thread-pane") && !closed.has_key(&format!("activity-reply-{reply}")),
        "closing a channel thread should return to that channel's roots, not All activity"
    );
}

#[test]
fn composer_search_individual_reply_and_reaction() {
    let f = Fixture::new();
    let tree = f.render(
        json!([
            {"t":0.1, "type":"key", "cmd":true, "name":"n"},
            {"t":0.3, "key":"destination", "type":"tap"},
            {"t":0.6, "type":"key", "cmd":true, "name":"a"},
            {"t":0.8, "paste":"studio/build"},
            {"t":1.0, "key":"message-body", "type":"tap"},
            {"t":1.3, "paste":"Native build report"},
            {"t":1.6, "type":"key", "cmd":true, "name":"Enter"}
        ]),
        "920x690",
        "1.9",
    );
    tree.expect_text("Native build report");
    let db = Board::open(&f.db).unwrap();
    let root = db
        .threads(&Filter::default(), 2)
        .unwrap()
        .pop()
        .unwrap()
        .message;
    assert_eq!(
        (&root.text[..], &root.channel[..], &root.actor[..]),
        ("Native build report", "studio/build", "reader")
    );
    let parent = db
        .reply(
            root.id,
            "casey/builder",
            "Unique match inside a reply",
            &PostOptions {
                labels: [("stage".to_owned(), "tested".to_owned())].into(),
                ..Default::default()
            },
        )
        .unwrap();
    let t = f.render(
        json!([
            {"t":0.1,"type":"tap","key":"nav-studio:"},
            {"t":0.4,"type":"key","cmd":true,"name":"f"},
            {"t":0.7,"paste":"unique MATCH"},
            {"t":0.9,"type":"key","name":"Enter"},
            {"t":1.2,"type":"tap","key":format!("star-{}", parent.id)},
            {"t":1.5,"type":"tap","key":format!("reply-{}", parent.id)},
            {"t":1.8,"paste":"Confirmed from the inspector"},
            {"t":2.1,"type":"key","cmd":true,"name":"Enter"}
        ]),
        "920x690",
        "2.6",
    );
    t.expect_text("Confirmed from the inspector");
    t.expect_text("stage=tested");
    t.expect_key(&format!("reaction-{}-0", parent.id));
    assert_eq!(
        db.get(parent.id).unwrap().reactions.get("star").unwrap(),
        &["reader"]
    );
    let messages = db.thread(root.id).unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(
        (messages[2].re, messages[2].reply_to),
        (Some(root.id), Some(parent.id))
    );
}

#[test]
fn inline_channel_composer_sends_with_enter_preserves_shift_newline_and_waits_for_ime_commit() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    b.create_channel("work").unwrap();
    let mut cmd=f.command(json!([
        {"t":0.12,"type":"tap","key":"nav-work"},{"t":0.35,"type":"tap","key":"message-body"},
        {"t":0.55,"paste":"First line"},{"t":0.75,"type":"key","shift":true,"name":"Enter"},
        {"t":0.95,"paste":"Second line"},{"t":1.15,"type":"key","name":"Enter"},
        {"t":1.45,"type":"tap","key":"nav-work"},{"t":1.65,"type":"tap","key":"message-body"},
        {"t":1.85,"paste":"Greeting: "},{"t":2.05,"type":"ime","preedit":"こんにちは","cursor":[15,15]},
        {"t":2.25,"type":"key","name":"Enter"},{"t":2.45,"type":"mark","name":"ime-held"},
        {"t":2.95,"type":"ime","commit":"こんにちは"},{"t":3.15,"type":"key","cmd":true,"name":"Enter"}
    ]),DESKTOP,"3.65");
    let mut interim = Vec::new();
    let t = f.at_mark(&mut cmd, "ime-held", || {
        interim = b.messages(&Filter::default(), 10).unwrap();
    });
    assert_eq!(
        interim.len(),
        1,
        "Enter during preedit published an unfinished message"
    );
    assert_eq!(interim[0].text, "First line\nSecond line");
    assert_eq!(interim[0].channel, "work");
    t.expect_text("Greeting: こんにちは");
    let msgs = b.messages(&Filter::default(), 10).unwrap();
    assert_eq!(
        msgs.iter()
            .map(|m| (&*m.channel, &*m.text, m.reply_to))
            .collect::<Vec<_>>(),
        [
            ("work", "First line\nSecond line", None),
            ("work", "Greeting: こんにちは", None)
        ]
    );
}

#[test]
fn split_composers_keep_root_and_reply_drafts_separate_across_focus_send_and_restart() {
    let f = Fixture::new();
    let db = Board::open(&f.db).unwrap();
    let root = db
        .post("desk", "writer", "Handoff", &Default::default())
        .unwrap()
        .id;
    let main = "Independent top-level note";
    let reply = "Reply from the thread";
    let mut cmd = f.command(json!([
        {"t":0.12,"type":"tap","key":"nav-desk"},
        {"t":0.32,"type":"tap","key":format!("thread-{root}")},
        {"t":0.55,"type":"tap","key":"message-body"},{"t":0.75,"paste":reply},
        {"t":0.98,"type":"tap","key":"feed-compose"},{"t":1.2,"type":"tap","key":"message-body"},
        {"t":1.42,"paste":main},{"t":1.65,"type":"tap","key":"thread-compose"},
        {"t":1.9,"type":"mark","name":"drafts-separated"},
        {"t":2.45,"type":"tap","key":"message-body"},{"t":2.62,"type":"key","name":"End"},
        {"t":2.85,"type":"key","shift":true,"name":"Enter"},{"t":3.05,"paste":"Second line"},
        {"t":3.3,"type":"key","name":"Enter"},{"t":3.7,"type":"tap","key":"feed-compose"}
    ]), DESKTOP, "4.15");
    let scope = format!("thread:{root}");
    let mut pending = None;
    let sent = f.at_mark(&mut cmd, "drafts-separated", || {
        pending = Some((
            db.load_draft("reader", &scope),
            db.load_draft("reader", "channel:desk"),
            db.thread(root).unwrap().len(),
        ));
    });
    let (thread_draft, root_draft, count) = pending.unwrap();
    let thread_draft = thread_draft
        .unwrap()
        .expect("reply persisted on pane switch");
    let root_draft = root_draft.unwrap().expect("root persisted on pane switch");
    assert_eq!(count, 1, "switching pane published a draft");
    assert_eq!(thread_draft.body, reply);
    assert_eq!(
        (
            root_draft.body.as_str(),
            root_draft.channel.as_str(),
            root_draft.reply_to
        ),
        (main, "desk", None)
    );
    sent.expect_key("thread-pane");
    sent.expect_key("thread-compose");
    sent.expect_text(main);
    let msgs = db.thread(root).unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(
        (msgs[1].text.as_str(), msgs[1].re, msgs[1].reply_to),
        ("Reply from the thread\nSecond line", Some(root), None)
    );
    assert!(db.load_draft("reader", &scope).unwrap().is_none());
    assert_eq!(
        db.load_draft("reader", "channel:desk")
            .unwrap()
            .unwrap()
            .body,
        main
    );
    assert!(db.load_draft("reader", "new").unwrap().is_none());
    let restored = f.render(
        json!([{"t":0.12,"type":"tap","key":"nav-desk"}]),
        DESKTOP,
        "0.5",
    );
    restored.expect_key("message-body");
    restored.expect_text(main);
    f.render(
        json!([
            {"t":0.12,"type":"tap","key":"nav-desk"},{"t":0.34,"type":"tap","key":"message-body"},{"t":0.6,"type":"key","name":"Enter"}
        ]),
        DESKTOP,
        "0.85",
    );
    let msgs = db.messages(&Filter::default(), 10).unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(
        (
            msgs[2].text.as_str(),
            msgs[2].channel.as_str(),
            msgs[2].re,
            msgs[2].reply_to
        ),
        (main, "desk", None, None)
    );
    assert!(db.load_draft("reader", "channel:desk").unwrap().is_none());
}

#[test]
fn live_inspector_renders_task_and_observes_an_independent_writer() {
    let f = Fixture::new();
    let board = Board::open(&f.db).unwrap();
    let root = board
        .post(
            "work",
            "jules/lead",
            "Verify the release",
            &Default::default(),
        )
        .unwrap();
    let task = create_task(
        &board,
        "jules/lead",
        TaskTarget::Existing(root.id),
        &[],
        Some("jules/build"),
    )
    .unwrap();
    board
        .post(
            "work",
            "jules/lead",
            "A later non-task thread",
            &Default::default(),
        )
        .unwrap();
    let png = f.dir.path().join("live.png");
    let mut cmd = f.command(
        json!([
            {"t":0.1,"type":"tap","key":"filter-messages"},
            {"t":0.55,"type":"tap","key":"tasks-only"},
            {"t":0.9,"type":"key","cmd":true,"name":"f"},
            {"t":1.2,"type":"key","name":"Enter"},
            {"t":1.45,"type":"mark","name":"opened"}
        ]),
        "900x660",
        "4.1",
    );
    let t = f.at_mark(cmd.arg("--snap").arg(&png), "opened", || {
        change_task(
            &board,
            task.id,
            "jules/lead",
            None,
            None,
            TaskAction::Assign(Some("jules/review")),
        )
        .unwrap();
        board
            .reply(
                root.id,
                "casey/build",
                "External writer checked in",
                &Default::default(),
            )
            .unwrap();
    });
    t.expect_key("task-details");
    for text in [
        "Verify the release",
        "ready",
        task.owner.as_str(),
        "jules/review",
        "External writer checked in",
    ] {
        t.expect_text(text);
    }
    assert!(
        t.0.iter()
            .any(|e| e["text"] == "lead" && e["help"] == "jules/lead")
    );
    check_pixels(&png, (900, 660));
}

#[test]
fn narrow_empty_no_matches_and_open_error() {
    let mut f = Fixture::new();
    let t = f.render(json!([]), "390x520", "0.2");
    t.expect_key("open-nav");
    t.expect_text("No threads yet");
    let board = Board::open(&f.db).unwrap();
    board
        .post("test", "taylor", "Message", &Default::default())
        .unwrap();
    let t = f.render(
        json!([
            {"t":0.1,"type":"tap","key":"open-nav"},
            {"t":0.4,"type":"tap","key":"nav-test"},
            {"t":0.7,"type":"key","cmd":true,"name":"f"},
            {"t":1.0,"paste":"nothing like this"}
        ]),
        "390x520",
        "1.3",
    );
    t.expect_text("No matching threads");
    f.db = f.dir.path().to_owned(); // A directory is not a SQLite file.
    let error = f.render(json!([]), "390x520", "0.2");
    error.expect_key("retry-open");
    error.expect_text("Board unavailable");
}

#[test]
fn older_pages_remain_browsable() {
    let f = Fixture::new();
    let db = Board::open(&f.db).unwrap();
    for n in 0..51 {
        db.post("log", "sam", &format!("Activity {n}"), &Default::default())
            .unwrap();
    }
    let t = f.render(
        json!([
            {"t":0.1, "type":"tap", "key":"older-threads"}
        ]),
        "780x540",
        "0.5",
    );
    t.expect_text("Page 2");
    t.expect_text("Activity 0");
    assert!(!t.0.iter().any(|e| e["text"] == "Activity 1"));
    let t = f.render(
        json!([
            {"t":0.1, "type":"tap", "key":"older-threads"},
            {"t":0.5, "type":"tap", "key":"newer-threads"}
        ]),
        "780x540",
        "0.9",
    );
    t.expect_text("Page 1");
    t.expect_text("Activity 50");
    let opened = f.render(
        json!([
            {"t":0.1, "type":"key", "cmd":true, "name":"f"},
            {"t":0.4, "type":"key", "name":"ArrowDown"},
            {"t":0.7, "type":"key", "name":"ArrowUp"},
            {"t":1.0, "type":"key", "name":"Enter"}
        ]),
        "780x540",
        "1.4",
    );
    opened.expect_key("conversation");
    opened.expect_text("Activity 49");
}

#[test]
fn renders_markdown_and_validated_blocks_with_a_json_toggle() {
    let f = Fixture::new();
    let board = Board::open(&f.db).unwrap();
    let sha = "a5".repeat(20);
    let data = json!({"title": "Reviewed change", "sha": sha});
    // A quoted example using the very same schema must remain ordinary code.
    let text = format!(
        "## Build notes\n\nBefore **the block**.\n\n> ```json#git-commit@1\n> example only\n> ```\n\n~~~json#git-commit@1\n{data}\n~~~\n\nAfter the block."
    );
    let m = board
        .post("dev", "reader", &text, &Default::default())
        .unwrap();
    assert_eq!(m.blocks.len(), 1);
    let open = json!({"t":0.1,"type":"tap","key":format!("thread-{}",m.id)});
    let t = f.render(json!([open]), "890x810", "0.5");
    t.expect_key(&format!("raw-{}-0", m.id));
    t.expect_text("Build notes");
    t.expect_text("example only");
    t.expect_text("git-commit@1");
    t.expect_text("Reviewed change");
    t.expect_text("After the block.");
    let t = f.render(
        json!([open,{"t":0.5,"type":"tap","key":format!("raw-{}-0",m.id)}]),
        "890x810",
        "0.9",
    );
    t.expect_text(&serde_json::to_string_pretty(&data).unwrap());
}

#[test]
fn metadata_revision_checked_edit_and_confirmed_tombstone() {
    let f = Fixture::new();
    let board = Board::open(&f.db).unwrap();
    let m = board
        .post(
            "dev",
            "reader/build",
            "Draft from agent",
            &Default::default(),
        )
        .unwrap();
    let reply = board
        .reply(m.id, "sam", "Keep this reply", &Default::default())
        .unwrap();
    let thread = format!("thread-{}", m.id);
    let inspect = format!("inspect-{}", m.id);
    let t = f.render(
        json!([
            {"t":0.1,"type":"tap","key":thread},
            {"t":0.4,"type":"tap","key":inspect},
            {"t":0.6,"type":"tap","key":"label-key"},
            {"t":0.8,"paste":"stage"},
            {"t":1.0,"type":"tap","key":"label-value"},
            {"t":1.2,"paste":"verified"},
            {"t":1.4,"type":"tap","key":"set-label"},
            {"t":1.7,"type":"tap","key":"segment1"},
            {"t":1.9,"paste":"eyes"},
            {"t":2.1,"type":"tap","key":"add-reaction"},
            {"t":2.3,"type":"tap","key":"segment2"},
            {"t":2.5,"type":"key","cmd":true,"name":"a"},
            {"t":2.7,"paste":"Edited **by owner**"},
            {"t":2.9,"type":"key","cmd":true,"name":"Enter"}
        ]),
        "820x650",
        "3.2",
    );
    t.expect_key("conversation");
    t.expect_text("stage=verified");
    t.expect_key(&format!("reaction-{}-0", m.id));
    t.expect_text("👀 1");
    let after = board.get(m.id).unwrap();
    assert_eq!(after.reactions.get("eyes").unwrap(), &["reader"]);
    assert_eq!(after.text, "Edited **by owner**");
    assert!(after.revision > m.revision);
    let t = f.render(
        json!([
            {"t":0.1,"type":"tap","key":thread},
            {"t":0.3,"type":"tap","key":inspect},
            {"t":0.5,"type":"tap","key":"label-row-0"},
            {"t":0.7,"type":"tap","key":"unset-label"},
            {"t":0.9,"type":"tap","key":"segment1"},
            {"t":1.1,"type":"tap","key":"reaction-row-0"},
            {"t":1.3,"type":"tap","key":"remove-reaction"},
            {"t":1.5,"type":"tap","key":"segment2"},
            {"t":1.7,"type":"tap","key":"delete-message"}
        ]),
        "430x620",
        "2",
    );
    t.expect_text(&format!("Delete message #{}?", m.id));
    t.expect_key("keep-message");
    let after = board.get(m.id).unwrap();
    assert!(!after.deleted && after.labels.is_empty() && after.reactions.is_empty());
    let t = f.render(
        json!([
            {"t":0.1,"type":"tap","key":thread},
            {"t":0.3,"type":"tap","key":inspect},
            {"t":0.5,"type":"tap","key":"segment2"},
            {"t":0.7,"type":"tap","key":"delete-message"},
            {"t":0.9,"type":"tap","key":"confirm-delete"}
        ]),
        "430x620",
        "1.2",
    );
    t.expect_text("Message deleted");
    t.expect_text("Keep this reply");
    assert!(board.get(m.id).unwrap().deleted);
    assert_eq!(board.get(reply.id).unwrap().re, Some(m.id));
}

#[test]
fn open_editor_rejects_an_intervening_process_write() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let m = b
        .post("dev", "reader", "Starting body", &Default::default())
        .unwrap();
    let mut cmd = f.command(
        json!([
            {"t":0.1,"type":"tap","key":format!("thread-{}",m.id)},
            {"t":0.4,"type":"tap","key":format!("inspect-{}",m.id)},
            {"t":0.6,"type":"tap","key":"segment2"},
            {"t":0.8,"type":"key","cmd":true,"name":"a"},
            {"t":1.0,"paste":"My draft"},
            {"t":1.2,"type":"mark","name":"editing"},
            {"t":2.6,"type":"tap","key":"save-edit"}
        ]),
        "820x650",
        "3.2",
    );
    let t = f.at_mark(&mut cmd, "editing", || {
        b.edit(m.id, "reader", "Another process won", Some(m.revision))
            .unwrap();
    });
    t.expect_key("reload-message");
    t.expect_key("edit-body");
    assert!(t.0.iter().any(|e| {
        e["text"]
            .as_str()
            .is_some_and(|v| v.contains("revision_conflict") || v.contains("current revision is"))
    }));
    assert_eq!(b.get(m.id).unwrap().text, "Another process won");
}

#[test]
fn native_file_dialog_and_drop_round_trip_attachment_bytes() {
    let f = Fixture::new();
    let first = f.dir.path().join("evidence.bin");
    let second = f.dir.path().join("notes.txt");
    let saved = f.dir.path().join("export.bin");
    let bytes = [0u8, 0xfe, 0xff, 3];
    fs::write(&first, bytes).unwrap();
    fs::write(&second, "local").unwrap();
    let t = f.render(
        json!([
            {"t":0.1,"type":"key","cmd":true,"name":"n"},
            {"t":0.3,"type":"tap","key":"attach-file"},
            {"t":0.6,"type":"file-dialog","path":first},
            {"t":0.9,"drop_files":{"paths":[second],"key":"message-body"}},
            {"t":1.2,"type":"key","name":"Escape"},
            {"t":1.5,"type":"key","cmd":true,"name":"n"},
            {"t":1.8,"type":"tap","key":"send"}
        ]),
        "820x700",
        "2.2",
    );
    t.expect_text("evidence.bin (4 bytes)");
    t.expect_text("notes.txt (5 bytes)");
    let board = Board::open(&f.db).unwrap();
    let m = board
        .threads(&Filter::default(), 1)
        .unwrap()
        .pop()
        .unwrap()
        .message;
    assert!(m.text.is_empty());
    assert_eq!(board.download(m.attachments[0].id).unwrap().1, bytes);
    let t = f.render(
        json!([
            {"t":0.1,"type":"tap","key":format!("thread-{}",m.id)},
            {"t":0.4,"type":"tap","key":format!("save-file-{}",m.attachments[0].id)},
            {"t":0.7,"type":"file-dialog","path":saved},
            {"t":1.0,"type":"tap","key":format!("save-file-{}",m.attachments[1].id)},
            {"t":1.3,"type":"file-dialog","path":saved}
        ]),
        "820x700",
        "1.6",
    );
    assert_eq!(fs::read(saved).unwrap(), bytes);
    assert!(t.0.iter().any(|e| {
        e["text"]
            .as_str()
            .is_some_and(|v| v.contains("choose a new filename"))
    }));
}

#[test]
fn label_editor_uses_the_value_seen_not_the_polled_value() {
    let f = Fixture::new();
    let board = Board::open(&f.db).unwrap();
    let m = board
        .post(
            "work",
            "other",
            "Decision",
            &PostOptions {
                labels: [("status".into(), "open".into())].into(),
                ..Default::default()
            },
        )
        .unwrap();
    let mut cmd = f.command(
        json!([
            {"t":0.1,"type":"tap","key":format!("thread-{}",m.id)},
            {"t":0.4,"type":"tap","key":format!("inspect-{}",m.id)},
            {"t":0.6,"type":"tap","key":"label-row-0"},
            {"t":0.8,"type":"key","cmd":true,"name":"a"},
            {"t":1.0,"paste":"mine"},
            {"t":1.2,"type":"mark","name":"label"},
            {"t":2.6,"type":"tap","key":"set-label"}
        ]),
        "620x630",
        "3.2",
    );
    let t = f.at_mark(&mut cmd, "label", || {
        board
            .label(m.id, "other", "status", Some("merged"))
            .unwrap();
    });
    assert_eq!(board.get(m.id).unwrap().labels["status"], "merged");
    t.expect_key("reload-message");
    assert!(t.0.iter().any(|e| {
        e["text"]
            .as_str()
            .is_some_and(|v| v.contains("label_conflict"))
    }));
    assert!(!t.0.iter().any(|e| e["text"] == "Edit"));
}

#[test]
fn chronological_activity_and_profile_read_position_survive_restart() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let root = b
        .post("ops", "reader", "First post", &Default::default())
        .unwrap();
    let reply = b
        .reply(
            root.id,
            "worker",
            "Reply in the stream",
            &Default::default(),
        )
        .unwrap();
    let t = f.render(
        json!([{"t":0.1,"type":"tap","key":"filter-messages"},{"t":0.55,"type":"tap","key":"activity"}]),
        DESKTOP,
        "1.0",
    );
    t.expect_key(&format!("activity-message-{}", root.id));
    t.expect_key(&format!("activity-message-{}", reply.id));
    t.expect_text("Reply in the stream");
    let t = f.render(
        json!([
            {"t":0.1,"type":"tap","key":"filter-messages"},{"t":0.55,"type":"tap","key":"activity"},
            {"t":0.9,"type":"tap","key":"segment2"},
            {"t":1.25,"type":"tap","key":"mark-read"}
        ]),
        DESKTOP,
        "1.65",
    );
    t.expect_text("0 unread");
    assert_eq!(b.activity_seen("reader").unwrap(), b.cursor().unwrap());
    assert_eq!(b.activity_seen("reader/build").unwrap(), 0);
    for n in 0..45 {
        b.reply(
            root.id,
            "reader",
            &format!("Own update {n}"),
            &Default::default(),
        )
        .unwrap();
    }
    let new_message = b
        .reply(
            root.id,
            "other",
            "Arrived after the mark",
            &Default::default(),
        )
        .unwrap();
    let t = f.render(
        json!([
            {"t":0.1,"type":"tap","key":"filter-messages"},{"t":0.55,"type":"tap","key":"activity"},
            {"t":0.9,"type":"tap","key":"segment2"}
        ]),
        DESKTOP,
        "1.3",
    );
    t.expect_text("1 unread");
    t.expect_text("Arrived after the mark");
    assert!(!t.0.iter().any(|e| e["text"] == "First post"));
    b.delete(new_message.id, "other", Some(new_message.revision))
        .unwrap();
    let t = f.render(
        json!([
            {"t":0.1,"type":"tap","key":"filter-messages"},{"t":0.55,"type":"tap","key":"activity"},
            {"t":0.9,"type":"tap","key":"segment2"}
        ]),
        DESKTOP,
        "1.3",
    );
    t.expect_text("Message deleted");
    assert!(!t.0.iter().any(|e| e["text"] == "Arrived after the mark"));
}

#[test]
fn switching_profiles_keeps_composer_drafts_and_authorship_separate() {
    let f = Fixture::new();
    let t = f.render(
        json!([
            {"t":0.1,"type":"key","cmd":true,"name":"n"},
            {"t":0.3,"paste":"Personal notes"},
            {"t":0.5,"type":"tap","key":"choose-profile"},
            {"t":0.7,"type":"tap","key":"profile-name"},
            {"t":0.9,"paste":"build"},
            {"t":1.1,"type":"tap","key":"switch-profile"},
            {"t":1.3,"type":"key","cmd":true,"name":"n"},
            {"t":1.5,"paste":"Profile report"},
            {"t":1.7,"type":"key","cmd":true,"name":"Enter"},
            {"t":2.0,"type":"tap","key":"choose-profile"},
            {"t":2.2,"type":"tap","key":"profile-0"},
            {"t":2.5,"type":"key","cmd":true,"name":"n"},
            {"t":2.8,"type":"key","cmd":true,"name":"Enter"}
        ]),
        DESKTOP,
        "3.1",
    );
    t.expect_text("Personal notes");
    let b = Board::open(&f.db).unwrap();
    let rows = b.messages(&Filter::default(), 5).unwrap();
    assert_eq!(
        rows.iter()
            .map(|m| (m.actor.as_str(), m.text.as_str()))
            .collect::<Vec<_>>(),
        [
            ("reader/build", "Profile report"),
            ("reader", "Personal notes")
        ]
    );
}

#[test]
fn task_board_claims_and_completes_using_a_fenced_lease() {
    let f = Fixture::new();
    let t = f.render(
        json!([
            {"t":0.1,"type":"tap","key":"tasks"},
            {"t":0.3,"type":"tap","key":"new-task"},
            {"t":0.6,"paste":"Build the artifact"},
            {"t":0.9,"type":"tap","key":"create-task"},
            {"t":1.2,"type":"tap","key":"claim-task"},
            {"t":1.5,"type":"tap","key":"renew-task"},
            {"t":1.8,"type":"tap","key":"release-task"},
            {"t":2.1,"type":"tap","key":"claim-task"},
            {"t":2.75,"type":"tap","key":"task-options"},
            {"t":3.10,"type":"tap","key":"task-result"},
            {"t":3.40,"paste":"Checks passed"},
            {"t":3.70,"type":"tap","key":"complete-task"}
        ]),
        DESKTOP,
        "4.1",
    );
    t.expect_text("completed");
    t.expect_text("Checks passed");
    let b = Board::open(&f.db).unwrap();
    let rows = board_local::automation::list_tasks(&b, None, Some("all")).unwrap();
    assert_eq!(rows.len(), 1);
    let task = &rows[0];
    assert_eq!(
        (&task.text[..], task.state.as_str(), task.epoch),
        ("Build the artifact", "completed", 2)
    );
    assert!(task.claim_token.is_none());
    let t = f.render(
        json!([
            {"t":0.1,"type":"tap","key":"tasks"},
            {"t":0.3,"type":"tap","key":"segment3"},
            {"t":0.6,"type":"tap","key":format!("task-row-{}",task.id)}
        ]),
        DESKTOP,
        "1.2",
    );
    t.expect_key("conversation");
    t.expect_key("task-detail-scroll");
    t.expect_text("Checks passed");
}

#[test]
fn administers_schedules_watchers_and_local_schemas() {
    let f = Fixture::new();
    let t = f.render(
        json!([
            {"t":0.1,"type":"tap","key":"workspace"},
            {"t":0.3,"type":"tap","key":"work-schedules"},
            {"t":0.5,"type":"tap","key":"add-resource"},
            {"t":0.7,"type":"tap","key":"resource-body"},
            {"t":0.9,"paste":"Future check"},
            {"t":1.1,"type":"tap","key":"save-resource"},
            {"t":1.3,"type":"tap","key":"remove-1"},
            {"t":1.5,"type":"tap","key":"confirm-1"},
            {"t":1.7,"type":"tap","key":"workspace"},
            {"t":1.9,"type":"tap","key":"work-watchers"},
            {"t":2.1,"type":"tap","key":"add-resource"},
            {"t":2.3,"type":"tap","key":"resource-name"},
            {"t":2.5,"paste":"observer"},
            {"t":2.7,"type":"tap","key":"resource-channel"},
            {"t":2.9,"paste":"dev/"},
            {"t":3.1,"type":"tap","key":"save-resource"}
        ]),
        "910x860",
        "3.5",
    );
    t.expect_key("watcher-observer");
    t.expect_text("observer");
    let b = Board::open(&f.db).unwrap();
    assert_eq!(
        board_local::automation::get_schedule(&b, 1).unwrap().state,
        "canceled"
    );
    assert_eq!(
        board_local::automation::get_watcher(&b, "observer")
            .unwrap()
            .filter
            .channel
            .as_deref(),
        Some("dev/")
    );
    let t = f.render(
        json!([
            {"t":0.1,"type":"tap","key":"workspace"},
            {"t":0.3,"type":"tap","key":"work-schemas"},
            {"t":0.5,"type":"tap","key":"add-resource"},
            {"t":0.7,"type":"tap","key":"resource-name"},
            {"t":0.9,"paste":"build-result"},
            {"t":1.1,"type":"tap","key":"save-resource"},
            {"t":1.4,"type":"tap","key":"schema-build-result-1"}
        ]),
        "910x860",
        "1.7",
    );
    t.expect_text("build-result@1");
    assert!(t.0.iter().any(|e| {
        e["text"]
            .as_str()
            .is_some_and(|v| v.contains("\"type\": \"object\""))
    }));
    assert!(
        b.schemas()
            .unwrap()
            .iter()
            .any(|schema| schema.name == "build-result")
    );
}

#[test]
fn delegated_inspector_filters_reads_and_removes_contents_on_revocation() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let allowed = b
        .post("public", "owner", "Public thread", &Default::default())
        .unwrap();
    b.post("private", "owner", "Private thread", &Default::default())
        .unwrap();
    b.set_grant(
        "owner",
        None,
        "guest/view",
        &board_local::access::Rules::parse("public=read").unwrap(),
    )
    .unwrap();
    let delegation = b
        .issue_credential("owner", "guest/view", None, None)
        .unwrap();
    let mut cmd = f.command(json!([]), DESKTOP, "0.5");
    let result = cmd.env("BOARD_TOKEN", &delegation.token).output().unwrap();
    assert!(result.status.success());
    let t = f.tree();
    t.expect_text("as guest/view");
    t.expect_text("Public thread");
    assert!(
        !t.0.iter()
            .any(|e| e["text"] == "Private thread" || e["key"] == "nav-private")
    );
    let mut cmd = f.command(
        json!([
            {"t":0.1,"type":"tap","key":"choose-profile"}
        ]),
        DESKTOP,
        "0.5",
    );
    assert!(
        cmd.env("BOARD_TOKEN", &delegation.token)
            .output()
            .unwrap()
            .status
            .success()
    );
    let t = f.tree();
    t.expect_text("Identity fixed by local credential");
    assert!(!t.0.iter().any(|e| e["key"] == "switch-profile"));
    let mut cmd = f.command(
        json!([
            {"t":0.1,"type":"tap","key":format!("thread-{}",allowed.id)},
            {"t":0.7,"type":"mark","name":"bound"}
        ]),
        DESKTOP,
        "3.0",
    );
    let t = f.at_mark(cmd.env("BOARD_TOKEN", &delegation.token), "bound", || {
        b.revoke_credential("owner", &delegation.credential.id)
            .unwrap();
    });
    assert!(!t.0.iter().any(|e| e["text"] == "Public thread"));
    assert!(
        t.0.iter()
            .any(|e| e["text"].as_str().is_some_and(|v| v.contains("revoked")))
    );
}

#[test]
fn owner_creates_grants_and_revokes_credentials_without_displaying_secrets() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let issued = b
        .issue_credential("reader", "reader/scan", None, None)
        .unwrap();
    let t = f.render(
        json!([
            {"t":0.1,"type":"tap","key":"workspace"},
            {"t":0.3,"type":"tap","key":"work-access"},
            {"t":0.5,"type":"tap","key":"add-resource"},
            {"t":0.7,"type":"tap","key":"resource-name"},
            {"t":0.9,"paste":"reader/scan"},
            {"t":1.1,"type":"tap","key":"resource-body"},
            {"t":1.3,"paste":"dev/=read,post"},
            {"t":1.5,"type":"tap","key":"save-resource"}
        ]),
        DESKTOP,
        "1.8",
    );
    t.expect_text("Grant #1    reader/scan");
    t.expect_text(&issued.credential.id);
    assert!(!t.0.iter().any(|e| e.to_string().contains(&issued.token)));
    assert_eq!(b.grants().unwrap().len(), 1);
    let id = &issued.credential.id;
    let t = f.render(
        json!([
            {"t":0.1,"type":"tap","key":"workspace"},
            {"t":0.3,"type":"tap","key":"work-access"},
            {"t":0.5,"type":"tap","key":"remove-grant-1"},
            {"t":0.7,"type":"tap","key":"confirm-grant-1"},
            {"t":0.9,"type":"tap","key":format!("remove-credential-{id}")},
            {"t":1.1,"type":"tap","key":format!("confirm-credential-{id}")}
        ]),
        DESKTOP,
        "1.4",
    );
    t.expect_text("Revoked    reader/scan");
    assert!(b.grants().unwrap().is_empty());
    assert!(b.credentials().unwrap()[0].revoked);
}

#[test]
fn typing_is_local_ephemeral_and_cleared_after_reply() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let m = b
        .post("ops", "reader", "Check status", &Default::default())
        .unwrap();
    let head = b.cursor().unwrap();
    b.set_typing(m.id, "worker", Some("reviewing"), 100)
        .unwrap();
    let mut cmd = f.command(
        json!([
            {"t":0.1,"type":"tap","key":format!("thread-{}",m.id)},
            {"t":0.4,"type":"tap","key":"message-body"},
            {"t":0.7,"paste":"Finished"},
            {"t":1.0,"type":"mark","name":"typing"},
            {"t":2.5,"type":"key","cmd":true,"name":"Enter"}
        ]),
        "850x710",
        "3.0",
    );
    let t = f.at_mark(&mut cmd, "typing", || {
        assert_eq!(head, b.cursor().unwrap());
        assert!(
            b.typing(m.id)
                .unwrap()
                .iter()
                .any(|p| p.actor == "reader" && p.status == "typing")
        );
    });
    t.expect_text("Finished");
    t.expect_key("typing");
    t.expect_text("worker: reviewing");
    assert!(!b.typing(m.id).unwrap().iter().any(|p| p.actor == "reader"));
}

#[test]
fn scoped_source_inbox_is_read_only_and_hides_raw_payloads_and_tokens() {
    use board_local::sources::{Change, Direction, Envelope, FailureReport, Import, Provider};
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let actor = "reader/bridge";
    b.create_source(actor, "local-review", Provider::Local, "builds", "dev")
        .unwrap();
    b.create_source(
        actor,
        "hidden-review",
        Provider::Local,
        "private-mapping",
        "private",
    )
    .unwrap();
    let envelope = |key: &str| Envelope {
        key: key.into(),
        import: Import {
            links: Vec::new(),
            no_sync: false,
            mentioned: Default::default(),
            thread: key.into(),
            external: key.into(),
            author: "worker".into(),
            profile: None,
            parent: None,
            url: None,
            updated_at: None,
            change: Change::Post {
                text: "Imported note".into(),
                labels: Default::default(),
            },
        },
        raw: json!({"Authorization":"NEVER-SHOW-RAW-CREDENTIAL"}),
    };
    let pending = b
        .enqueue_imports(actor, "local-review", &[envelope("pending")], None)
        .unwrap()[0];
    let done = b
        .import_source(actor, "local-review", &envelope("complete"))
        .unwrap();
    let local = b
        .post("dev", "reader", "Local thread", &Default::default())
        .unwrap();
    b.set_outbound(actor, "local-review", true).unwrap();
    let uncertain = b
        .start_source_thread(actor, "local-review", local.id)
        .unwrap();
    let lease = b
        .take_source_delivery(actor, "local-review", Direction::Out, 60)
        .unwrap()
        .unwrap();
    b.fail_source(
        actor,
        "local-review",
        lease.job.id,
        &lease.receipt,
        FailureReport {
            reason: "NEVER-SHOW-FAILURE-CREDENTIAL",
            uncertain: true,
            retry_after: None,
        },
    )
    .unwrap();
    let later = b
        .post("dev", "reader", "Second local thread", &Default::default())
        .unwrap();
    let failed = b
        .start_source_thread(actor, "local-review", later.id)
        .unwrap();
    for attempt in 1..=5 {
        let l = b
            .take_source_delivery(actor, "local-review", Direction::Out, 60)
            .unwrap()
            .unwrap();
        assert_eq!(l.job.id, failed.id);
        b.fail_source(
            actor,
            "local-review",
            failed.id,
            &l.receipt,
            FailureReport {
                reason: "NEVER-SHOW-FAILURE-CREDENTIAL",
                uncertain: false,
                retry_after: Some(1),
            },
        )
        .unwrap();
        if attempt < 5 {
            std::thread::sleep(std::time::Duration::from_millis(1050));
        }
    }
    b.set_grant(
        "reader",
        None,
        actor,
        &board_local::access::Rules::parse("dev=read,sync").unwrap(),
    )
    .unwrap();
    let issued = b.issue_credential("reader", actor, None, None).unwrap();
    let head = b.cursor().unwrap();
    let base = json!([
        {"t":0.1,"type":"tap","key":"workspace"},{"t":0.3,"type":"tap","key":"work-sources"}
    ]);
    let render = |steps: Value, end: &str| {
        let mut cmd = f.command(steps, DESKTOP, end);
        let result = cmd.env("BOARD_TOKEN", &issued.token).output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let t = f.tree();
        for secret in [
            &*issued.token,
            &*lease.receipt,
            "NEVER-SHOW-RAW-CREDENTIAL",
            "NEVER-SHOW-FAILURE-CREDENTIAL",
            "hidden-review",
            "private-mapping",
        ] {
            assert!(
                !t.0.iter().any(|e| e.to_string().contains(secret)),
                "source view disclosed a private field"
            );
        }
        t
    };
    let t = render(base.clone(), "0.5");
    t.expect_key("source-local-review");
    let mut select = base.as_array().unwrap().clone();
    select.push(json!({"t":0.6,"type":"tap","key":"source-local-review"}));
    let mut rows = select.clone();
    let done_job = b
        .inbox(actor, "local-review", Direction::In, 0, 40)
        .unwrap()
        .into_iter()
        .find(|j| j.result == Some(done.id))
        .unwrap();
    rows.push(json!({"t":0.9,"type":"tap","key":format!("job-{}",done_job.id)}));
    let t = render(Value::Array(rows), "1.2");
    t.expect_key(&format!("job-{pending}"));
    t.expect_text("pending    Attempts: 0");
    t.expect_text("Linked copies: 1");
    select.extend([
        json!({"t":0.9,"type":"tap","key":"segment1"}),
        json!({"t":1.2,"type":"tap","key":format!("job-{}",uncertain.id)}),
    ]);
    let t = render(Value::Array(select), "1.5");
    t.expect_text("External outcome unknown");
    t.expect_text("uncertain    Attempts: 1");
    t.expect_text("failed    Attempts: 5");
    assert_eq!(head, b.cursor().unwrap());
    assert_eq!(
        b.queue_item(actor, "local-review", pending).unwrap().state,
        "pending"
    );
    assert_eq!(
        b.queue_item(actor, "local-review", uncertain.id)
            .unwrap()
            .state,
        "uncertain"
    );
}

#[test]
fn deep_link_and_actor_draft_restore_after_restart() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let root = b
        .post("dev", "other", "Deep link destination", &Default::default())
        .unwrap();
    let reply = b
        .reply(
            root.id,
            "other/worker",
            "Target this reply",
            &Default::default(),
        )
        .unwrap();
    let pos = b.cursor().unwrap();
    let run = |steps: Value, end: &str| {
        let mut cmd = f.command(steps, "880x710", end);
        let result = cmd
            .arg("--thread")
            .arg(reply.id.to_string())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        f.tree()
    };
    let t = run(
        json!([
            {"t":0.1,"type":"tap","key":format!("reply-{}",reply.id)},
            {"t":0.4,"paste":"Draft survives the restart"}
        ]),
        "1.4",
    );
    t.expect_text("Draft survives the restart");
    let scope = format!("thread:{}", root.id);
    let draft = b.load_draft("reader", &scope).unwrap().unwrap();
    assert_eq!(
        (draft.body.as_str(), draft.reply_to),
        ("Draft survives the restart", Some(reply.id))
    );
    assert!(b.load_draft("reader/worker", &scope).unwrap().is_none());
    assert_eq!(pos, b.cursor().unwrap());
    let t = run(json!([{"t":0.2,"type":"tap","key":"send"}]), "0.6");
    t.expect_text("Draft survives the restart");
    assert!(b.load_draft("reader", &scope).unwrap().is_none());
    assert_eq!(
        b.thread(root.id).unwrap().last().unwrap().reply_to,
        Some(reply.id)
    );
    let error = Command::new(env!("CARGO_BIN_EXE_board-ui"))
        .args(["--thread", "nope"])
        .output()
        .unwrap();
    assert_eq!(error.status.code(), Some(2));
}

fn run_at(f: &Fixture, steps: Value, message: i64, end: &str) -> Tree {
    let out = f
        .command(steps, "900x720", end)
        .arg("--thread")
        .arg(message.to_string())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    f.tree()
}

#[test]
fn guarded_reply_pins_visible_id_across_external_write_restart_and_explicit_reselection() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let root = b
        .post("jobs", "owner", "Release", &Default::default())
        .unwrap()
        .id;
    let seen = b
        .reply(root, "owner", "Ready?", &Default::default())
        .unwrap()
        .id;
    let mut cmd = f.command(json!([
        {"t":0.2,"type":"tap","key":format!("inspect-{seen}")},{"t":0.5,"type":"tap","key":"guard-reply"},
        {"t":0.8,"paste":"Ready to release"},{"t":1.0,"type":"mark","name":"writing"},
        {"t":3.4,"type":"tap","key":"send"}
    ]),"900x780","3.8");
    cmd.arg("--thread").arg(root.to_string());
    let t = f.at_mark(&mut cmd, "writing", || {
        Board::open(&f.db)
            .unwrap()
            .reply(seen, "another", "Stop for review", &Default::default())
            .unwrap();
    });
    let all = b.thread(root).unwrap();
    assert_eq!(all.len(), 3);
    let newest = all.last().unwrap().id;
    let expected = format!("Only if #{seen} is still newest");
    t.expect_text(&expected);
    t.expect_text(&format!(
        "newer_message: latest message is #{newest}, not #{seen}"
    ));
    t.expect_text("Stop for review");
    let scope = format!("thread:{root}");
    let saved = b.load_composer("reader", &scope).unwrap().unwrap();
    assert_eq!(saved.settings.after, Some(seen));
    assert_eq!(saved.draft.body, "Ready to release");
    let t = run_at(
        &f,
        json!([{"t":0.2,"type":"tap","key":"send"}]),
        root,
        "0.6",
    );
    t.expect_text(&expected);
    assert_eq!(b.thread(root).unwrap().len(), 3);
    let t = run_at(
        &f,
        json!([
            {"t":0.2,"type":"tap","key":format!("inspect-{newest}")},{"t":0.5,"type":"tap","key":"guard-reply"},
            {"t":0.8,"type":"key","cmd":true,"name":"Enter"}
        ]),
        root,
        "1.2",
    );
    t.expect_text("Ready to release");
    assert!(!t.0.iter().any(|e| e["key"] == "pinned-guard"));
    let all = b.thread(root).unwrap();
    assert_eq!(
        all.len(),
        4,
        "a continuously open independent reader must see the committed reply"
    );
    assert_eq!(all.last().unwrap().reply_to, Some(newest));
    assert!(b.load_composer("reader", &scope).unwrap().is_none());
}

#[test]
fn schema_examples_append_to_real_drafts_and_unsolved_shapes_offer_only_the_definition() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    b.register_schema("approval",3,"Local approval",&json!({"type":"object",
        "additionalProperties":false,"required":["answer"],"properties":{"answer":{"const":"pending"}}})).unwrap();
    b.register_schema(
        "impossible",
        1,
        "No accepted object",
        &json!({"type":"object","not":{"type":"object"}}),
    )
    .unwrap();
    let draft = board_local::Draft {
        body: "Keep this first".into(),
        channel: "decisions".into(),
        reply_to: None,
    };
    b.save_draft("reader", "new", Some(&draft)).unwrap();
    let steps = json!([
        {"t":0.2,"type":"tap","key":"workspace"},{"t":0.4,"type":"tap","key":"work-schemas"},
        {"t":0.6,"type":"tap","key":"schema-approval-3"},{"t":0.9,"type":"tap","key":"copy-schema-example"},
        {"t":1.2,"type":"tap","key":"compose-schema-example"},{"t":1.5,"type":"tap","key":"send"}
    ]);
    let t = f.render(steps, DESKTOP, "1.9");
    let msgs = b.messages(&Filter::default(), 10).unwrap();
    assert_eq!(msgs.len(), 1);
    let m = &msgs[0];
    assert_eq!(m.channel, "decisions");
    assert!(
        m.text
            .starts_with("Keep this first\n\n```json#approval@3\n")
    );
    assert_eq!(m.blocks.len(), 1);
    assert_eq!(m.blocks[0].data, json!({"answer":"pending"}));
    t.expect_text("approval@3");
    let t = f.render(json!([
        {"t":0.2,"type":"tap","key":"workspace"},{"t":0.4,"type":"tap","key":"work-schemas"},
        {"t":0.6,"type":"tap","key":"schema-impossible-1"},{"t":0.9,"type":"tap","key":"copy-schema-definition"}
    ]),DESKTOP,"1.2");
    t.expect_key("copy-schema-definition");
    t.expect_text("No validated example available");
    assert!(
        !t.0.iter()
            .any(|e| e["key"] == "copy-schema-example" || e["key"] == "compose-schema-example")
    );
}

#[test]
fn gif_webp_and_avif_previews_render_the_attachment_pixels() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let mut gif = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
        for (rgba, ms) in [([0, 255, 255, 255], 250), ([255, 0, 255, 255], 2000)] {
            encoder
                .encode_frame(image::Frame::from_parts(
                    image::RgbaImage::from_pixel(24, 16, image::Rgba(rgba)),
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(ms, 1),
                ))
                .unwrap();
        }
    }
    // Both animations start cyan. A magenta framebuffer proves we actually advanced.
    let mut files = vec![
        ("swatch.gif".into(), gif),
        (
            "swatch.webp".into(),
            include_bytes!("fixtures/animated.webp").to_vec(),
        ),
    ];
    if Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|s| s.status.success())
    {
        files.push((
            "swatch.avif".into(),
            include_bytes!("fixtures/magenta.avif").to_vec(),
        ));
    }
    let root = b
        .post(
            "studio",
            "artist",
            "Color proof",
            &PostOptions {
                files,
                ..Default::default()
            },
        )
        .unwrap();
    for attachment in root.attachments {
        let png = f.dir.path().join(format!("{}.png", attachment.name));
        let cmd = f.command(json!([{"t":0.2,"type":"tap","key":format!("preview-{}-{}",root.id,attachment.id)}]),"820x630","0.8")
            .arg("--thread").arg(root.id.to_string()).arg("--snap").arg(&png).output().unwrap();
        assert!(
            cmd.status.success(),
            "{}: {}",
            attachment.name,
            String::from_utf8_lossy(&cmd.stderr)
        );
        f.tree().expect_key("preview-image");
        f.tree().expect_text("24 x 16 px");
        let frame = image::open(&png).unwrap().into_rgb8();
        assert!(
            frame
                .pixels()
                .filter(|c| c[0] > 215 && c[1] < 35 && c[2] > 215)
                .count()
                > 8000,
            "{} didn't paint magenta",
            attachment.name
        );
    }
}

#[test]
fn local_raster_previews_render_pixels_and_never_overwrite_exports() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let mut bytes = Vec::new();
    let rgb: Vec<_> = (0..128)
        .flat_map(|p| {
            if p % 16 < 8 {
                [5, 236, 226]
            } else {
                [244, 15, 214]
            }
        })
        .collect();
    {
        let mut enc = png::Encoder::new(&mut bytes, 16, 8);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&rgb).unwrap();
    }
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
        .encode(&rgb, 16, 8, image::ExtendedColorType::Rgb8)
        .unwrap();
    let root = b
        .post(
            "design",
            "maker",
            "Swatches",
            &PostOptions {
                files: vec![
                    ("swatch.png".into(), bytes.clone()),
                    ("swatch.jpg".into(), jpeg),
                ],
                ..Default::default()
            },
        )
        .unwrap();
    let id = root.attachments[0].id;
    let shot = f.dir.path().join("raster.png");
    let saved = f.dir.path().join("copy.png");
    let out = f
        .command(
            json!([
                {"t":0.15,"type":"tap","key":format!("preview-{}-{id}",root.id)},
                {"t":0.5,"type":"tap","key":"export-preview"},
                {"t":0.8,"type":"file-dialog","path":saved}
            ]),
            "820x630",
            "1.1",
        )
        .arg("--thread")
        .arg(root.id.to_string())
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
    t.expect_key("preview-image");
    t.expect_text("16 x 8 px");
    assert_eq!(fs::read(&saved).unwrap(), bytes);
    let painted = image::open(&shot).unwrap().into_rgb8();
    for color in [[5_u8, 236, 226], [244, 15, 214]] {
        assert!(
            painted
                .pixels()
                .filter(|p| p.0.into_iter().zip(color).all(|(v, c)| v.abs_diff(c) < 12))
                .count()
                > 8_000,
            "attachment color {color:?} missing from actual Rune PNG"
        );
    }
    let t = run_at(
        &f,
        json!([
            {"t":0.2,"type":"tap","key":format!("preview-{}-{}",root.id,root.attachments[1].id)},
            {"t":0.4,"type":"tap","key":"export-preview"},
            {"t":0.7,"type":"file-dialog","path":saved}
        ]),
        root.id,
        "1.0",
    );
    t.expect_key("preview-image");
    assert!(t.0.iter().any(|e| {
        e["text"]
            .as_str()
            .is_some_and(|s| s.contains("choose a new filename"))
    }));
    assert_eq!(fs::read(&saved).unwrap(), bytes);
    let t = f.render(
        json!([
            {"t":0.1,"type":"key","cmd":true,"name":"n"},
            {"t":0.4,"paste":{"png":saved}},
            {"t":0.6,"type":"tap","key":"send"}
        ]),
        "830x680",
        "1.0",
    );
    t.expect_text(&format!("clipboard-1.png ({} bytes)", bytes.len()));
    let post = b
        .messages(
            &Filter {
                by: Some("reader".into()),
                ..Default::default()
            },
            1,
        )
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(b.download(post.attachments[0].id).unwrap().1, bytes);
}

#[test]
fn safe_preview_refuses_active_and_oversized_content_and_closes_on_tombstone() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let mut giant = Vec::new();
    {
        let enc = png::Encoder::new(&mut giant, 5000, 1);
        enc.write_header()
            .unwrap()
            .write_image_data(&[0; 5000])
            .unwrap();
    }
    let html = "<script>fetch('http://127.0.0.1/private')</script>";
    let m = b
        .post(
            "docs",
            "reader",
            "Review file",
            &PostOptions {
                files: vec![
                    ("page.html".into(), html.as_bytes().into()),
                    ("long.png".into(), giant),
                    ("readme.txt".into(), b"# Literal **text**".to_vec()),
                ],
                ..Default::default()
            },
        )
        .unwrap();
    let open = |i: usize| json!({"t":0.15,"type":"tap","key":format!("preview-{}-{}",m.id,m.attachments[i].id)});
    let t = run_at(&f, json!([open(0)]), m.id, "0.6");
    t.expect_text("No inline preview for this file type");
    assert!(!t.0.iter().any(|e| e.to_string().contains("127.0.0.1")));
    let t = run_at(&f, json!([open(1)]), m.id, "0.6");
    t.expect_key("preview-unavailable");
    assert!(!t.0.iter().any(|e| e["key"] == "preview-image"));
    let t = run_at(&f, json!([open(2)]), m.id, "0.6");
    t.expect_text("# Literal **text**");
    let mut cmd = f.command(
        json!([open(2),{"t":0.7,"type":"mark","name":"previewed"}]),
        "900x720",
        "2.6",
    );
    let t = f.at_mark(
        cmd.arg("--thread").arg(m.id.to_string()),
        "previewed",
        || {
            b.delete(m.id, "reader", Some(m.revision)).unwrap();
        },
    );
    t.expect_text("Message deleted");
    assert!(
        !t.0.iter()
            .any(|e| e["key"] == "preview-text" || e["text"] == "# Literal **text**")
    );
}

#[test]
fn keyboard_seek_and_bounded_bidirectional_reply_pages() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let root = b
        .post("dev", "reader", "Long running thread", &Default::default())
        .unwrap();
    let replies: Vec<_> = (1..=93)
        .map(|n| {
            b.reply(
                root.id,
                "worker",
                &format!("Observation {n:03}"),
                &Default::default(),
            )
            .unwrap()
            .id
        })
        .collect();
    let t = run_at(&f, json!([]), root.id, "0.6");
    t.expect_text("Observation 054");
    t.expect_text("Observation 093");
    assert!(!t.0.iter().any(|e| e["text"] == "Observation 053"));
    let target = replies[49];
    let t = f.render(
        json!([
            {"t":0.15,"type":"key","cmd":true,"name":"l"},{"t":0.4,"paste":format!("#{target}")},
            {"t":0.65,"type":"key","name":"Enter"}
        ]),
        "900x720",
        "1.25",
    );
    t.expect_text("Observation 050");
    t.expect_text("Observation 011");
    t.expect_text("Long running thread");
    let y = |key: &str| {
        let el = t.0.iter().find(|v| v["key"] == key).unwrap();
        (
            el["rect"][1].as_f64().unwrap(),
            el["rect"][3].as_f64().unwrap(),
        )
    };
    let (top, height) = y("conversation");
    let (target_y, _) = y(&format!("inspect-{target}"));
    assert!(
        (top..top + height).contains(&target_y),
        "jump did not reveal the addressed reply"
    );
    assert!(
        !t.0.iter()
            .any(|e| e["text"] == "Observation 051" || e["text"] == "Observation 010")
    );
    let t = run_at(
        &f,
        json!([{"t":0.2,"type":"tap","key":"older-replies"}]),
        target,
        "0.6",
    );
    t.expect_text("Observation 001");
    t.expect_text("Observation 010");
    assert!(!t.0.iter().any(|e| e["text"] == "Observation 011"));
    let t = run_at(
        &f,
        json!([{"t":0.2,"type":"tap","key":"newer-replies"}]),
        target,
        "0.6",
    );
    t.expect_text("Observation 051");
    t.expect_text("Observation 090");
    let t = run_at(
        &f,
        json!([{"t":0.2,"type":"tap","key":"latest-replies"}]),
        target,
        "0.6",
    );
    t.expect_text("Observation 093");
}

#[test]
fn query_filters_replies_before_paging_and_separates_imported_origins() {
    use board_local::sources::{Change, Envelope, Import, Provider};
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let root = b
        .post(
            "ops/build",
            "lead",
            "Target thread",
            &PostOptions {
                labels: [
                    ("stage".to_string(), "review".into()),
                    ("empty".into(), String::new()),
                ]
                .into(),
                ..Default::default()
            },
        )
        .unwrap();
    let reply = b
        .reply(
            root.id,
            "worker/run",
            "Need @reader signoff",
            &Default::default(),
        )
        .unwrap();
    for i in 0..65 {
        b.post(
            "ops/build",
            "worker/run",
            &format!("Distractor {i} @reader"),
            &Default::default(),
        )
        .unwrap();
    }
    b.create_source("reader", "mirror", Provider::Local, "inbox", "ops/build")
        .unwrap();
    let imp = b
        .import_source(
            "reader",
            "mirror",
            &Envelope {
                key: "in-1".into(),
                raw: json!({}),
                import: Import {
                    links: Vec::new(),
                    no_sync: false,
                    mentioned: Default::default(),
                    thread: "remote-1".into(),
                    external: "note-1".into(),
                    author: "bot".into(),
                    profile: None,
                    parent: None,
                    url: None,
                    updated_at: None,
                    change: Change::Post {
                        text: "Imported activity".into(),
                        labels: Default::default(),
                    },
                },
            },
        )
        .unwrap();
    let steps = json!([
        {"t":0.1,"type":"tap","key":"nav-ops:"},{"t":0.3,"type":"key","cmd":true,"shift":true,"name":"f"},
        {"t":0.5,"paste":"worker"},{"t":0.7,"type":"tap","key":"filter-mention"},{"t":0.9,"paste":"reader"},
        {"t":1.1,"type":"tap","key":"filter-text"},{"t":1.3,"paste":"signoff"},
        {"t":1.5,"type":"tap","key":"filter-labels"},{"t":1.7,"paste":"stage=ready|review\nempty="},
        {"t":1.9,"type":"tap","key":"apply-filters"}
    ]);
    let t = f.render(steps.clone(), DESKTOP, "2.2");
    t.expect_text("Target thread");
    assert!(!t.0.iter().any(|e| {
        e["text"]
            .as_str()
            .is_some_and(|s| s.starts_with("Distractor"))
    }));
    let mut activity = steps.as_array().unwrap().clone();
    activity.push(json!({"t":2.3,"type":"tap","key":"filter-messages"}));
    activity.push(json!({"t":2.5,"type":"tap","key":"activity"}));
    let t = f.render(Value::Array(activity), DESKTOP, "2.8");
    t.expect_key(&format!("activity-message-{}", reply.id));
    assert!(
        !t.0.iter()
            .any(|e| e["key"] == format!("activity-message-{}", root.id)),
        "unexpected root in filtered activity: {:?}",
        t.0.iter()
            .filter_map(|e| e["text"].as_str())
            .collect::<Vec<_>>()
    );
    let t=f.render(json!([
        {"t":0.1,"type":"tap","key":"filter-messages"},{"t":0.3,"type":"tap","key":"filter-from"},
        {"t":0.5,"paste":"mirror"},{"t":0.7,"type":"tap","key":"apply-filters"}
    ]),DESKTOP,"1.0");
    t.expect_text("Imported activity");
    t.expect_key(&format!("thread-{}", imp.id));
    assert!(!t.0.iter().any(|e| e["text"] == "Target thread"));
}

#[test]
fn directory_edits_local_presentation_and_composer_inserts_exact_mention() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    b.post("studio", "reader", "Start", &Default::default())
        .unwrap();
    b.set_actor_profile("artist/render", "shade", "Render worker")
        .unwrap();
    b.post(
        "studio",
        "artist/render",
        "Visible profile",
        &Default::default(),
    )
    .unwrap();
    let t=f.render(json!([
        {"t":0.1,"type":"tap","key":"choose-profile"},{"t":0.3,"type":"tap","key":"open-directory"},
        {"t":0.5,"type":"tap","key":"edit-person"},{"t":0.7,"type":"key","cmd":true,"name":"a"},{"t":0.9,"paste":"Studio Reader"},
        {"t":1.1,"type":"tap","key":"person-handle"},{"t":1.3,"type":"key","cmd":true,"name":"a"},{"t":1.5,"paste":"studio-reader"},
        {"t":1.7,"type":"tap","key":"save-person"}
    ]),DESKTOP,"2.1");
    t.expect_text("Studio Reader");
    t.expect_text("Render worker");
    assert_eq!(
        b.actor_directory(Some("studio-reader")).unwrap()[0].id,
        "reader"
    );
    let t = f.render(
        json!([
            {"t":0.1,"type":"key","cmd":true,"name":"n"},{"t":0.3,"paste":"Thanks "},
            {"t":0.5,"type":"tap","key":"mention-person"},{"t":0.7,"paste":"shade"},
            {"t":0.9,"type":"key","name":"Enter"},{"t":1.1,"type":"key","cmd":true,"name":"Enter"}
        ]),
        DESKTOP,
        "1.6",
    );
    t.expect_key("conversation");
    let sent = b
        .messages(
            &Filter {
                by: Some("reader".into()),
                mentions: Some("artist/render".into()),
                ..Filter::default()
            },
            5,
        )
        .unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].text.trim(), "Thanks @[artist/render]");
    b.set_grant(
        "reader",
        None,
        "guest",
        &board_local::access::Rules::parse("studio=read").unwrap(),
    )
    .unwrap();
    b.post("private", "hidden/user", "Hidden", &Default::default())
        .unwrap();
    let credential = b.issue_credential("reader", "guest", None, None).unwrap();
    let mut cmd=f.command(json!([
        {"t":0.1,"type":"tap","key":"choose-profile"},{"t":0.3,"type":"tap","key":"open-directory"}
    ]),DESKTOP,"0.6");
    assert!(
        cmd.env("BOARD_TOKEN", &credential.token)
            .output()
            .unwrap()
            .status
            .success()
    );
    let t = f.tree();
    t.expect_text("Render worker");
    assert!(!t.0.iter().any(|e| e.to_string().contains("hidden/user")));
}

#[test]
fn task_routing_filters_query_all_pages_and_pending_schedules_are_in_thread() {
    use board_local::automation::{Destination, Timing, schedule};
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let make = |name: &str, actor: &str, ch: &str, needs: &[i64], labels: PostOptions| {
        create_task(
            &b,
            actor,
            TaskTarget::New {
                channel: ch,
                text: name,
                options: &labels,
            },
            needs,
            Some(&format!("{actor}/builder")),
        )
        .unwrap()
    };
    let prereq = make("First job", "reader", "ops/work", &[], Default::default());
    for n in 0..43 {
        make(
            &format!("Other task {n}"),
            "other",
            "other",
            &[],
            Default::default(),
        );
    }
    let target = make(
        "Filtered job",
        "reader",
        "ops/work",
        &[prereq.id],
        PostOptions {
            labels: [("batch".into(), "nightly".into())].into(),
            ..Default::default()
        },
    );
    let t=f.render(json!([
        {"t":0.1,"type":"tap","key":"tasks"},{"t":0.3,"type":"tap","key":"task-criteria"},
        {"t":0.5,"type":"tap","key":"criteria-owner"},{"t":0.7,"paste":"reader"},
        {"t":0.9,"type":"tap","key":"criteria-assignee"},{"t":1.1,"paste":"reader/builder"},
        {"t":1.3,"type":"tap","key":"criteria-channel"},{"t":1.5,"paste":"ops/"},
        {"t":1.7,"type":"tap","key":"criteria-needs"},{"t":1.9,"paste":format!("#{}",prereq.id)},
        {"t":2.1,"type":"tap","key":"criteria-labels"},{"t":2.3,"paste":"batch=weekly|nightly"},
        {"t":2.5,"type":"tap","key":"apply-task-filters"}
    ]),DESKTOP,"2.8");
    t.expect_text("Filtered job");
    t.expect_key(&format!("task-row-{}", target.id));
    assert!(
        !t.0.iter()
            .any(|e| e["text"] == "First job" || e["key"] == "next-page")
    );
    let scheduled = schedule(
        &b,
        "reader",
        Destination::Reply(target.id),
        "Scheduled followup",
        Timing {
            due: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
                + 3600,
            every: None,
            times: None,
            until: None,
        },
        Default::default(),
    )
    .unwrap();
    let t = run_at(&f, json!([]), target.id, "0.6");
    t.expect_key(&format!("scheduled-reply-{}", scheduled.id));
}

fn stamp_in(seconds: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    chrono::DateTime::from_timestamp(now + seconds, 0)
        .unwrap()
        .to_rfc3339()
}

#[test]
fn scheduled_private_composer_recovers_files_and_cancels_in_thread() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let root = b
        .post("ops", "reader", "Plan", &Default::default())
        .unwrap();
    let parent = b
        .reply(root.id, "reader", "Target", &Default::default())
        .unwrap();
    let file = f.dir.path().join("sample.bin");
    fs::write(&file, [42, 0, 29]).unwrap();
    let scope = format!("thread:{}", root.id);
    let t = run_at(
        &f,
        json!([
            {"t":0.2,"type":"tap","key":format!("reply-{}",parent.id)},{"t":0.4,"paste":"Private recurring follow-up"},
            {"t":0.6,"type":"tap","key":"attach-file"},{"t":0.8,"type":"file-dialog","path":file},
            {"t":1.0,"type":"tap","key":"delivery-options"},{"t":1.2,"type":"tap","key":"compose-no-sync"},
            {"t":1.4,"type":"tap","key":"compose-labels"},{"t":1.6,"paste":"stage=classified"},
            {"t":1.8,"type":"tap","key":"compose-later"},
            {"t":2.0,"type":"tap","key":"compose-due"},{"t":2.2,"paste":stamp_in(3600)},
            {"t":2.4,"type":"tap","key":"compose-every"},{"t":2.6,"paste":"60"},
            {"t":2.8,"type":"tap","key":"compose-times"},{"t":3.0,"paste":"2"},
            {"t":3.2,"type":"tap","key":"done-delivery"}
        ]),
        parent.id,
        "3.6",
    );
    t.expect_text("Schedule");
    t.expect_text("sample.bin (3 bytes)");
    let saved = b.load_composer("reader", &scope).unwrap().unwrap();
    assert!(saved.settings.no_sync && saved.settings.later);
    assert_eq!(saved.draft.reply_to, Some(parent.id));
    assert_eq!(saved.files[0].1, [42, 0, 29]);
    let t = run_at(
        &f,
        json!([{"t":0.2,"type":"tap","key":"send"}]),
        root.id,
        "0.6",
    );
    let schedule = board_local::automation::schedules(&b, true)
        .unwrap()
        .pop()
        .unwrap();
    t.expect_key(&format!("scheduled-reply-{}", schedule.id));
    assert!(schedule.no_sync);
    assert_eq!(schedule.re, Some(parent.id));
    assert_eq!(schedule.labels["stage"], "classified");
    assert_eq!((schedule.every, schedule.max_sends), (Some(60), Some(2)));
    assert_eq!(schedule.attachments[0].size, 3);
    assert_eq!(b.thread(root.id).unwrap().len(), 2);
    assert!(b.load_composer("reader", &scope).unwrap().is_none());
    let t = run_at(
        &f,
        json!([
            {"t":0.2,"type":"tap","key":format!("cancel-pending-{}",schedule.id)},
            {"t":0.5,"type":"tap","key":format!("confirm-pending-{}",schedule.id)}
        ]),
        root.id,
        "0.8",
    );
    assert!(
        !t.0.iter()
            .any(|e| e["key"] == format!("scheduled-reply-{}", schedule.id))
    );
    assert_eq!(
        board_local::automation::get_schedule(&b, schedule.id)
            .unwrap()
            .state,
        "canceled"
    );
}

#[test]
fn recurring_composer_persists_stop_date_and_rejects_two_competing_stops() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let until = stamp_in(7200);
    let steps = json!([
        {"t":0.1,"type":"key","cmd":true,"name":"n"},{"t":0.4,"type":"tap","key":"message-body"},{"t":0.6,"paste":"Until a date"},
        {"t":0.8,"type":"tap","key":"delivery-options"},{"t":1.0,"type":"tap","key":"compose-later"},
        {"t":1.2,"type":"tap","key":"compose-due"},{"t":1.4,"paste":stamp_in(3600)},
        {"t":1.6,"type":"tap","key":"compose-every"},{"t":1.8,"paste":"60"},
        {"t":2.0,"type":"tap","key":"compose-times"},{"t":2.2,"paste":"8"},
        {"t":2.4,"type":"tap","key":"compose-until"},{"t":2.6,"paste":until},
        {"t":2.8,"type":"tap","key":"done-delivery"},{"t":3.0,"type":"tap","key":"send"}
    ]);
    let t = f.render(steps, "940x830", "3.3");
    assert!(t.0.iter().any(|e| {
        e["text"]
            .as_str()
            .is_some_and(|s| s.contains("only one repeat stop"))
    }));
    assert!(
        board_local::automation::schedules(&b, true)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        b.load_composer("reader", "new")
            .unwrap()
            .unwrap()
            .settings
            .until,
        until
    );
    let t=f.render(json!([
        {"t":0.1,"type":"key","cmd":true,"name":"n"},{"t":0.4,"type":"tap","key":"delivery-options"},
        {"t":0.7,"type":"tap","key":"compose-times"},{"t":0.9,"type":"key","cmd":true,"name":"a"},
        {"t":1.1,"type":"key","name":"Backspace"},{"t":1.3,"type":"tap","key":"done-delivery"},
        {"t":1.6,"type":"tap","key":"send"}
    ]),"940x830","2.0");
    let rows = board_local::automation::schedules(&b, true).unwrap();
    assert_eq!(rows.len(), 1);
    t.expect_key(&format!("schedule-{}", rows[0].id));
    assert_eq!(rows[0].max_sends, None);
    assert_eq!(
        rows[0].until,
        Some(
            chrono::DateTime::parse_from_rfc3339(&until)
                .unwrap()
                .timestamp()
        )
    );
    assert!(b.load_composer("reader", "new").unwrap().is_none());
}

#[test]
fn stale_composer_cannot_publish_after_privacy_change_or_downgrade_an_overdue_schedule() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let original = board_local::Draft {
        body: "Keep the classification".into(),
        channel: "ops".into(),
        reply_to: None,
    };
    let plain = board_local::ComposerSettings::default();
    let stamp = b
        .save_composer("reader", "new", None, Some((&original, &plain)), None)
        .unwrap()
        .unwrap();
    let mut cmd = f.command(
        json!([
            {"t":0.1,"type":"key","cmd":true,"name":"n"},{"t":0.5,"type":"mark","name":"loaded"},
            {"t":1.5,"type":"tap","key":"send"}
        ]),
        "860x720",
        "2.1",
    );
    let private = board_local::ComposerSettings {
        no_sync: true,
        later: true,
        due: stamp_in(-600),
        ..Default::default()
    };
    let t = f.at_mark(&mut cmd, "loaded", || {
        b.save_composer(
            "reader",
            "new",
            Some(&stamp),
            Some((&original, &private)),
            None,
        )
        .unwrap();
    });
    assert!(t.0.iter().any(|e| {
        e["text"]
            .as_str()
            .is_some_and(|t| t.contains("Draft changed"))
    }));
    assert!(b.messages(&Filter::default(), 5).unwrap().is_empty());
    let t = f.render(
        json!([{"t":0.1,"type":"key","cmd":true,"name":"n"},{"t":0.4,"type":"tap","key":"send"}]),
        "860x720",
        "0.7",
    );
    assert!(t.0.iter().any(|e| {
        e["text"]
            .as_str()
            .is_some_and(|t| t.contains("send time has passed"))
    }));
    assert!(b.messages(&Filter::default(), 5).unwrap().is_empty());
    assert!(
        board_local::automation::schedules(&b, true)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        b.load_composer("reader", "new").unwrap().unwrap().settings,
        private
    );
    let t=f.render(json!([
        {"t":0.1,"type":"key","cmd":true,"name":"n"},{"t":0.4,"type":"tap","key":"delivery-options"},
        {"t":0.7,"type":"tap","key":"compose-later"},{"t":1.0,"type":"tap","key":"done-delivery"},
        {"t":1.3,"type":"tap","key":"send"}
    ]),"860x720","1.6");
    t.expect_text("Local only");
    let messages = b.messages(&Filter::default(), 5).unwrap();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].no_sync);
}

#[test]
fn exact_message_delivery_failures_are_scoped_and_local_only_replies_are_not_queued() {
    use board_local::sources::{DeliveryResult, Direction, FailureReport, Provider};
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let root = b
        .post("dev", "reader", "Mirrored root", &Default::default())
        .unwrap();
    b.create_source("reader", "mirror", Provider::Local, "mirror-room", "dev")
        .unwrap();
    b.set_outbound("reader", "mirror", true).unwrap();
    let confirmed = b.start_source_thread("reader", "mirror", root.id).unwrap();
    let first = b
        .take_source_delivery("reader", "mirror", Direction::Out, 60)
        .unwrap()
        .unwrap();
    b.finish_source(
        "reader",
        "mirror",
        first.job.id,
        &first.receipt,
        Some(&DeliveryResult {
            external: "copy-1".into(),
            url: Some("https://mirror.invalid/items/1".into()),
            thread: Some("one".into()),
        }),
    )
    .unwrap();
    assert_eq!(
        b.source_copies("reader", "mirror", root.id).unwrap().len(),
        1
    );
    let t = run_at(
        &f,
        json!([
            {"t":0.2,"type":"tap","key":"message-body"},{"t":0.4,"paste":"Private reply"},
            {"t":0.6,"type":"tap","key":"delivery-options"},{"t":0.8,"type":"tap","key":"compose-no-sync"},
            {"t":1.0,"type":"tap","key":"done-delivery"},{"t":1.2,"type":"tap","key":"send"}
        ]),
        root.id,
        "1.6",
    );
    t.expect_text("Local only");
    let private = b.thread(root.id).unwrap().pop().unwrap();
    assert!(private.no_sync);
    assert!(
        b.message_deliveries("reader", private.id)
            .unwrap()
            .is_empty()
    );
    b.edit(
        root.id,
        "reader",
        "Mirrored root updated",
        Some(root.revision),
    )
    .unwrap();
    let edit = b
        .take_source_delivery("reader", "mirror", Direction::Out, 60)
        .unwrap()
        .unwrap();
    b.fail_source(
        "reader",
        "mirror",
        edit.job.id,
        &edit.receipt,
        FailureReport {
            reason: "PRIVATE RAW FAILURE SECRET",
            uncertain: true,
            retry_after: None,
        },
    )
    .unwrap();
    let t = run_at(
        &f,
        json!([{"t":0.2,"type":"tap","key":format!("inspect-{}",root.id)}]),
        root.id,
        "0.6",
    );
    for id in [confirmed.id, edit.job.id] {
        t.expect_key(&format!("delivery-notice-{id}"));
    }
    assert!(t.0.iter().any(|e| {
        e["text"]
            .as_str()
            .is_some_and(|s| s.contains("uncertain") && s.contains("Attempts: 1"))
    }));
    assert!(
        !t.0.iter().any(|e| e.to_string().contains(&edit.receipt)
            || e.to_string().contains("PRIVATE RAW FAILURE"))
    );
    let t = run_at(
        &f,
        json!([{"t":0.2,"type":"tap","key":format!("inspect-{}",private.id)}]),
        private.id,
        "0.6",
    );
    assert!(!t.0.iter().any(|e| {
        e["key"]
            .as_str()
            .is_some_and(|k| k.starts_with("delivery-notice-"))
    }));
    b.set_grant(
        "reader",
        None,
        "viewer",
        &board_local::access::Rules::parse("dev=read").unwrap(),
    )
    .unwrap();
    let token = b.issue_credential("reader", "viewer", None, None).unwrap();
    let mut cmd = f.command(
        json!([{"t":0.2,"type":"tap","key":format!("inspect-{}",root.id)}]),
        "860x700",
        "0.6",
    );
    assert!(
        cmd.env("BOARD_TOKEN", token.token)
            .arg("--thread")
            .arg(root.id.to_string())
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(!f.tree().0.iter().any(|e| {
        e["key"]
            .as_str()
            .is_some_and(|k| k.starts_with("delivery-notice-"))
    }));
    let quote = b
        .post(
            "dev",
            "reader",
            "Reference https://mirror.invalid/items/1",
            &Default::default(),
        )
        .unwrap();
    let t = run_at(&f, json!([]), quote.id, "0.5");
    t.expect_key(&format!("quote-{}-0", quote.id));
    t.expect_text(&format!("Quoted message #{}", root.id));
    t.expect_text("Mirrored root updated");
}

#[test]
fn opt_in_h264_playback_steps_real_frames_and_degrades_without_the_decoder() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let m = b
        .post(
            "video",
            "reader",
            "Color swatches",
            &PostOptions {
                files: vec![(
                    "motion.mp4".into(),
                    include_bytes!("fixtures/swatches.mp4").to_vec(),
                )],
                ..Default::default()
            },
        )
        .unwrap();
    let open =
        json!({"t":0.1,"type":"tap","key":format!("preview-{}-{}",m.id,m.attachments[0].id)});
    let mut missing = f.command(json!([open]), "820x640", "1.0");
    let output = missing
        .env("PATH", f.dir.path())
        .arg("--thread")
        .arg(m.id.to_string())
        .arg("--realtime")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    f.tree()
        .expect_text("Video playback needs local ffmpeg and ffprobe executables");
    if !video::available("h264") {
        return;
    }
    let shot = f.dir.path().join("frame.png");
    let mut cmd = f.command(
        json!([open,{"t":1.0,"type":"tap","key":"clip-next"},
        {"t":1.4,"type":"tap","key":"clip-play"}]),
        "820x640",
        "4.3",
    );
    let o = cmd
        .arg("--thread")
        .arg(m.id.to_string())
        .arg("--realtime")
        .arg("--snap")
        .arg(&shot)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let t = f.tree();
    t.expect_key("clip-frame");
    t.expect_text("Silent video   0:01 / 0:02   Frame 24 of 24");
    let painted = image::open(&shot).unwrap().into_rgb8();
    assert!(
        painted
            .pixels()
            .filter(|p| p.0[0] > 220 && p.0[1] < 30 && p.0[2] > 220)
            .count()
            > 7000,
        "the last decoded magenta frame must actually reach the Rune framebuffer"
    );
}

#[path = "inspector/sidebar.rs"]
mod sidebar;
