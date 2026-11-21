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

// Each shipped binary creates a whole graphics device, including a software
// renderer's thread pool on CPU-only builders. Bound those devices independently
// of libtest's CPU-based thread count.
static RENDERERS: (Mutex<u8>, Condvar) = (Mutex::new(3), Condvar::new());
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
            .env_remove("BOARD_PROFILE")
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

impl Tree {
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

fn check_pixels(path: &Path, size: (u32, u32)) {
    let mut reader = png::Decoder::new(BufReader::new(File::open(path).unwrap()))
        .read_info()
        .unwrap();
    let mut rgba = vec![0; reader.output_buffer_size().unwrap()];
    let frame = reader.next_frame(&mut rgba).unwrap();
    assert_eq!((frame.width, frame.height), size);
    assert_eq!(frame.color_type, png::ColorType::Rgba);
    let (pixels, remainder) = rgba[..frame.buffer_size()].as_chunks::<4>();
    assert!(remainder.is_empty());
    let ink = pixels
        .iter()
        .filter(|p| p[0] < 100 && p[1] < 100 && p[2] < 100 && p[3] == 255)
        .count();
    let accent = pixels
        .iter()
        .filter(|p| p[0] > 135 && p[1] < 100 && p[2] < 105 && p[3] == 255)
        .count();
    assert!(
        ink > 180 && accent > 100,
        "blank or missing content: {ink} ink pixels, {accent} accent pixels"
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
    t.expect_text("star 1 (reader)");
    let messages = db.thread(root.id).unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(
        (messages[2].re, messages[2].reply_to),
        (Some(root.id), Some(parent.id))
    );
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
            {"t":0.1,"type":"tap","key":"tasks-only"},
            {"t":0.4,"type":"key","cmd":true,"name":"f"},
            {"t":0.7,"type":"key","name":"Enter"},
            {"t":0.9,"type":"mark","name":"opened"}
        ]),
        "900x660",
        "3.2",
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
        "jules/lead",
        "jules/review",
        "External writer checked in",
    ] {
        t.expect_text(text);
    }
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
            {"t":0.7, "type":"key", "name":"ArrowDown"},
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
    t.expect_text("eyes 1 (reader)");
    let after = board.get(m.id).unwrap();
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
        json!([{"t":0.1,"type":"tap","key":"activity"}]),
        "850x680",
        "0.4",
    );
    t.expect_key(&format!("activity-message-{}", root.id));
    t.expect_key(&format!("activity-message-{}", reply.id));
    t.expect_text("Reply in the stream");
    let t = f.render(
        json!([
            {"t":0.1,"type":"tap","key":"activity"},
            {"t":0.4,"type":"tap","key":"segment2"},
            {"t":0.7,"type":"tap","key":"mark-read"}
        ]),
        "850x680",
        "1",
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
            {"t":0.1,"type":"tap","key":"activity"},
            {"t":0.3,"type":"tap","key":"segment2"}
        ]),
        "850x680",
        "0.7",
    );
    t.expect_text("1 unread");
    t.expect_text("Arrived after the mark");
    assert!(!t.0.iter().any(|e| e["text"] == "First post"));
    b.delete(new_message.id, "other", Some(new_message.revision))
        .unwrap();
    let t = f.render(
        json!([
            {"t":0.1,"type":"tap","key":"activity"},
            {"t":0.3,"type":"tap","key":"segment2"}
        ]),
        "850x680",
        "0.7",
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
        "880x690",
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
            {"t":2.4,"type":"tap","key":"task-result"},
            {"t":2.7,"paste":"Checks passed"},
            {"t":3.0,"type":"tap","key":"complete-task"}
        ]),
        "900x900",
        "3.3",
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
            {"t":0.6,"type":"tap","key":format!("task-row-{}",task.id)},
            {"t":0.9,"type":"tap","key":"task-thread"}
        ]),
        "850x680",
        "1.2",
    );
    t.expect_key("conversation");
    t.expect_key("task-details");
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
    let mut cmd = f.command(json!([]), "870x690", "0.5");
    let result = cmd.env("BOARD_TOKEN", &delegation.token).output().unwrap();
    assert!(result.status.success());
    let t = f.tree();
    t.expect_text("guest/view");
    t.expect_text("Public thread");
    assert!(
        !t.0.iter()
            .any(|e| e["text"] == "Private thread" || e["key"] == "nav-private")
    );
    let mut cmd = f.command(
        json!([
            {"t":0.1,"type":"tap","key":"choose-profile"}
        ]),
        "870x690",
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
        "870x690",
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
        "890x760",
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
        "890x760",
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
        let mut cmd = f.command(steps, "880x770", end);
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
