mod delivery;
mod filters;
mod icons;
mod messages;
mod people;
mod preview;
mod properties;
mod rich;
mod shell;
mod suggestions;
mod workspace;

use crate::state::{Page, Position, State};
use board_local::{Message, ThreadSummary, automation::Task, local};
use messages::{author, conversation, feed, message_text, stamp, task_details};
use rune::{
    Color, Cursor, Cx, El, Icon, Key, ScrollSpec, TextStyle, Tx, col, el, icon, row, text,
    text_input,
};
use rune_kit::{Kit, glyphs};
use std::cell::Cell;

const BG: &str = "#fafafa";
const INK: &str = "#040404";
const DIM: &str = "#303030";
const MUTED: &str = "#646464";
const FAINT: &str = "#909090";
const EDGE: &str = "#d9d9d9";
const TINT: &str = "#f0f0f0";
const AVATAR: &str = "#e3e3e3";
const ERROR: &str = "#c70000";
const ACCENT: &str = INK;
const PANEL: &str = TINT;

// Rune builds each window's element tree synchronously on its UI thread.
thread_local! { static DARK: Cell<bool> = const { Cell::new(false) }; }
fn dark() -> bool {
    DARK.get()
}
fn c(token: &str) -> Color {
    let hex = if dark() {
        match token {
            BG => "#040404",
            INK => "#fafafa",
            DIM => "#dedede",
            MUTED => "#909090",
            FAINT => "#646464",
            EDGE => "#252525",
            TINT => "#161616",
            AVATAR => "#252525",
            ERROR => "#ff7b72",
            v => v,
        }
    } else {
        token
    };
    rune::color::hex(hex).expect("static colour")
}
fn font(size: f32, weight: f32) -> TextStyle {
    TextStyle {
        family: rune::Family::SystemMono,
        ..TextStyle::new(size, weight)
    }
}
fn label(s: impl Into<String>, size: f32, weight: f32, ink: &str) -> El {
    text(s).font(font(size, weight)).color(c(ink))
}
fn mono(s: impl Into<String>, ink: &str) -> El {
    label(s, 11.0, 400.0, ink)
}
fn rule() -> El {
    el().w_full().h(1.0).fill(c(EDGE))
}
fn click(base: El, name: &str) -> El {
    base.role("button")
        .label(name)
        .focusable(true)
        .focus_highlight(c(INK).alpha(0.10))
        .cursor(Cursor::Default)
        .hover(c(INK).alpha(0.045))
        .press(c(INK).alpha(0.09))
}
fn tool(key: &str, name: &str, glyph: &'static Icon) -> El {
    click(el().key(key).size(28.0, 28.0).center().help(name), name)
        .child(icon(glyph, 15.0).color(c(MUTED)))
}
fn action(key: &str, name: &str, primary: bool) -> El {
    click(
        row()
            .key(key)
            .h(32.0)
            .center()
            .gap(6.0)
            .px(10.0)
            .fill(c(if primary { INK } else { TINT })),
        name,
    )
    .child(label(name, 12.0, 450.0, if primary { BG } else { DIM }))
}
fn avatar_size(actor: &str, size: f32) -> El {
    let initial = actor
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string();
    el().size(size, size)
        .radius(4.0)
        .fill(c(AVATAR))
        .center()
        .child(label(initial, 12.0, 600.0, MUTED))
}
fn avatar(actor: &str) -> El {
    avatar_size(actor, 28.0)
}

pub fn compose(s: &mut State, tx: &mut Tx) {
    s.go(Page::Compose);
    if s.page == Page::Compose {
        tx.focus_when_shown("message-body", true);
    }
}

fn scroll(key: &str, inner: El) -> El {
    col()
        .key(key)
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .child(inner)
}
fn title_for(s: &State) -> String {
    s.channel
        .as_ref()
        .map(|ch| format!("#{ch}"))
        .unwrap_or_else(|| "activity".into())
}
fn search(s: &State) -> El {
    let mut field = row()
        .grow(1.0)
        .min_w(0.0)
        .h(35.0)
        .center_y()
        .gap(9.0)
        .px(10.0)
        .border(1.0, c(EDGE))
        .fill(c(TINT))
        .child(icon(&glyphs::SEARCH, 15.0).color(c(MUTED)))
        .child(
            text_input(&s.query, font(13.0, 450.0))
                .single_line()
                .key("search")
                .label("Search threads")
                .placeholder("Search threads", c(MUTED))
                .grow(1.0)
                .min_w(0.0)
                .on_edit(|s: &mut State| &mut s.query)
                .on_input_change(|s: &mut State, _| {
                    s.reset_feed();
                    s.refresh();
                })
                .on_input_key(|s: &mut State, k, tx| {
                    let Some(first) = s.threads.last() else {
                        return false;
                    };
                    let id = first.message.id;
                    match k.key {
                        Key::Enter => s.go(Page::Thread(id)),
                        Key::Down => tx.focus(format!("thread-{id}"), true),
                        _ => return false,
                    }
                    true
                }),
        );
    if !s.query.text.is_empty() {
        field = field.child(
            tool("clear-search", "Clear search", &glyphs::CLOSE).on_press(|s: &mut State| {
                s.query.clear();
                s.reset_feed();
                s.refresh();
            }),
        );
    }
    field
}

pub fn view(s: &State, cx: &mut Cx<State>) -> El {
    let preference = THEME.get_or_init(|| match std::env::var("BOARD_UI_THEME").as_deref() {
        Ok("light") => Some(false),
        Ok("dark") => Some(true),
        _ => None,
    });
    DARK.set(preference.unwrap_or_else(|| cx.system_dark()));
    cx.foreground(c(DIM));
    cx.caption_color(c(BG));
    cx.focus_ring(1.0, 1.0, c(INK));
    let width = cx.layout_with("board-window", |r| r[2]).unwrap_or(1000.0);
    shell::window(s, width)
}
static THEME: std::sync::OnceLock<Option<bool>> = std::sync::OnceLock::new();
