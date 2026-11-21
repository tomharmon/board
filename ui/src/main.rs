#![forbid(unsafe_code)]

mod state;
mod view;

use rune::{AppSpec, Key, KeyEvent, Tx};
use state::{Page, State};
use std::sync::OnceLock;

static THREAD: OnceLock<i64> = OnceLock::new();
fn initial() -> State {
    let mut s = State::open();
    if s.connected()
        && let Some(id) = THREAD.get()
    {
        s.open_message_thread(*id);
    }
    s
}

fn key(s: &mut State, k: &KeyEvent, tx: &mut Tx) -> bool {
    if k.is_command('=') || k.is_command('+') {
        s.zoom_step(1);
    } else if k.is_command('-') {
        s.zoom_step(-1);
    } else if k.is_command('0') {
        s.zoom_step(0);
    } else if k.is_command('n') && s.connected() {
        view::compose(s, tx);
    } else if k.is_command('l') && s.connected() {
        s.jump_field(tx);
    } else if k.is_command('f') && k.mods.shift && s.connected() {
        if !matches!(s.page, Page::Feed | Page::Activity) {
            s.go(Page::Feed);
        } else if s.page == Page::Activity {
            s.desk.mode = state::ActivityMode::Messages;
        }
        s.show_filters(tx);
    } else if k.is_command('f') {
        s.open_search(tx);
    } else if k.key == Key::Escape && s.browse.jump_open {
        s.browse.jump_open = false;
        tx.focus_when_shown("jump-message", true);
    } else if k.key == Key::Escape && s.browse.filter_open {
        s.browse.filter_open = false;
        tx.focus_when_shown("filter-messages", true);
    } else if k.key == Key::Escape && s.people.picking {
        s.people = state::People::default();
        tx.focus_when_shown("message-body", true);
    } else if k.key == Key::Escape && s.delivery.open {
        s.delivery.open = false;
        tx.focus_when_shown("delivery-options", true);
    } else if k.key == Key::Escape && s.nav {
        s.nav = false;
        tx.focus_when_shown("open-nav", true);
    } else if k.key == Key::Escape && s.properties.is_some() {
        s.close_properties(tx);
    } else if k.key == Key::Escape && s.preview.is_some() {
        s.close_preview(tx);
    } else if k.key == Key::Escape && matches!(s.page, Page::Task(_)) && s.desk.task_options {
        s.set_task_options(false);
        tx.focus_when_shown("task-options", true);
    } else if k.key == Key::Escape && matches!(s.page, Page::Thread(_)) {
        s.close_thread(tx);
    } else if k.key == Key::Escape && s.browse.search_open {
        s.close_search();
        tx.focus_when_shown("search-toggle", true);
    } else if k.key == Key::Escape && s.page != Page::Feed {
        let old = s.page;
        s.back();
        if s.page == old {
            return true;
        }
        let target = match (s.page, old) {
            (Page::Feed, Page::Thread(id)) => format!("thread-{id}"),
            (Page::Tasks, Page::Task(id)) => format!("task-row-{id}"),
            (Page::Thread(_), Page::Task(_)) => "open-task".into(),
            (Page::Workspace, _) => "work-sections".into(),
            (Page::Activity, _) => "activity-modes".into(),
            _ => "new-thread".into(),
        };
        tx.focus_when_shown(target, true);
    } else {
        return false;
    }
    true
}

fn main() {
    if std::env::args_os().skip(1).eq(["--ui-notices"]) {
        print!(
            "Rune, rune-kit and rune-highlights\n{}\nEmbedded Inter fonts\n{}\nPierre Highlights (translated by Rune)\n{}\nLucide glyphs\n{}",
            include_str!("../licenses/Rune-MIT.txt"),
            include_str!("../licenses/Inter-OFL.txt"),
            include_str!("../licenses/Highlights-Apache.txt"),
            include_str!("../licenses/Lucide.txt")
        );
        return;
    }
    if std::env::args_os().any(|arg| arg == "--help" || arg == "-h") {
        println!(
            "Board options:\n  --thread ID     Open the local thread containing this message\n  --ui-notices    Print bundled UI licenses"
        );
    }
    match rune::tooling::extra_args() {
        [] => (),
        [flag, id] if flag == "--thread" && id.parse::<i64>().is_ok_and(|n| n > 0) => {
            let _ = THREAD.set(id.parse().expect("validated ID"));
        }
        _ => {
            eprintln!("usage: board-ui [--thread POSITIVE_ID] [Rune options]");
            std::process::exit(2);
        }
    }
    rune::set_app_id("org.localboard.Board");
    let app = AppSpec {
        min_size: Some((380.0, 420.0)),
        poll: Some(State::poll),
        on_key: Some(key),
        ..AppSpec::new("Board", (1000.0, 690.0), initial, view::view)
    };
    // Our layout starts at the top of the client area; the OS owns its titlebar.
    rune::App::new(app)
        .chrome(rune::Chrome {
            full_size_content: false,
            ..rune::Chrome::DEFAULT
        })
        .run();
}
