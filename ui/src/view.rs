mod delivery;
mod filters;
mod people;
mod preview;
mod properties;
mod rich;
mod workspace;

use crate::state::{Page, Position, State};
use board_local::{Message, ThreadSummary, automation::Task, local};
use rune::{
    Color, Cursor, Cx, El, Icon, Key, ScrollSpec, TextStyle, Tx, col, el, icon, row, text,
    text_input,
};
use rune_kit::{Checkbox, Kit, glyphs};
use std::collections::BTreeMap;

const INK: &str = "#252728";
const MUTED: &str = "#62686a";
const EDGE: &str = "#dce0dd";
const ACCENT: &str = "#a93640";
const PANEL: &str = "#f4f5f2";
const TINT: &str = "#f4e5e4";

fn c(hex: &str) -> Color {
    rune::color::hex(hex).expect("static colour")
}
fn label(s: impl Into<String>, size: f32, weight: f32, ink: &str) -> El {
    text(s).font(TextStyle::new(size, weight)).color(c(ink))
}
fn mono(s: impl Into<String>, ink: &str) -> El {
    text(s)
        .font(TextStyle {
            family: rune::Family::SystemMono,
            ..TextStyle::new(11.5, 500.0)
        })
        .color(c(ink))
}
fn rule() -> El {
    el().w_full().h(1.0).fill(c(EDGE))
}
fn click(base: El, name: &str) -> El {
    base.role("button")
        .label(name)
        .focusable(true)
        .focus_highlight(Color::BLACK.alpha(0.055))
        .cursor(Cursor::Pointer)
        .hover(Color::BLACK.alpha(0.045))
        .press(Color::BLACK.alpha(0.08))
}
fn tool(key: &str, name: &str, glyph: &'static Icon) -> El {
    click(
        el().key(key)
            .size(30.0, 30.0)
            .center()
            .radius(5.0)
            .help(name),
        name,
    )
    .child(icon(glyph, 16.0).color(c(MUTED)))
}
fn action(key: &str, name: &str, primary: bool) -> El {
    click(
        row()
            .key(key)
            .h(34.0)
            .center()
            .gap(8.0)
            .px(12.0)
            .radius(5.0)
            .fill(c(if primary { ACCENT } else { "#ffffff" }))
            .border(1.0, c(if primary { ACCENT } else { EDGE })),
        name,
    )
    .child(label(
        name,
        13.0,
        600.0,
        if primary { "#ffffff" } else { INK },
    ))
}
fn logomark() -> El {
    let mut paper = col()
        .size(29.0, 29.0)
        .justify_center()
        .pl(7.0)
        .gap(3.0)
        .radius(5.0)
        .fill(c(ACCENT));
    for w in [15.0, 10.0, 13.0] {
        paper = paper.child(el().w(w).h(2.0).fill(Color::WHITE));
    }
    paper
}
fn avatar(actor: &str) -> El {
    let palette = [
        ("#e1e9f2", "#304f78"),
        ("#e2ede0", "#355c42"),
        ("#ebe4f1", "#65507a"),
        ("#faead8", "#735422"),
    ];
    let i = actor
        .bytes()
        .fold(0_usize, |i, b| i.wrapping_add(b as usize))
        % palette.len();
    let (bg, fg) = palette[i];
    let initial = actor
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string();
    el().size(28.0, 28.0)
        .radius(6.0)
        .fill(c(bg))
        .center()
        .child(label(initial, 13.0, 600.0, fg))
}

pub fn compose(s: &mut State, tx: &mut Tx) {
    s.go(Page::Compose);
    tx.focus_when_shown("message-body", true);
}

fn sidebar(s: &State, narrow: bool) -> El {
    let mut top = row()
        .w_full()
        .center_y()
        .gap(10.0)
        .h(68.0)
        .px(18.0)
        .child(logomark())
        .child(label("Board", 21.0, 650.0, INK).grow(1.0));
    if narrow {
        top = top.child(
            tool("close-nav", "Close channels", &glyphs::CLOSE)
                .on_press(|s: &mut State| s.nav = false),
        );
    }
    let highlight = |selected| {
        if selected {
            c(TINT)
        } else {
            Color::TRANSPARENT
        }
    };
    let navitem = |name: &str, count: i64, selected, key: &str, prefix: El| {
        click(
            row()
                .key(key)
                .w_full()
                .h(34.0)
                .px(9.0)
                .center_y()
                .gap(9.0)
                .radius(5.0)
                .fill(highlight(selected)),
            name,
        )
        .child(prefix)
        .child(
            label(
                name,
                13.0,
                if selected { 600.0 } else { 450.0 },
                if selected { ACCENT } else { INK },
            )
            .grow(1.0)
            .min_w(0.0)
            .truncate(),
        )
        .child(mono(
            if count >= 0 {
                count.to_string()
            } else {
                String::new()
            },
            MUTED,
        ))
    };
    let mut items = col()
        .w_full()
        .role("list")
        .focus_group()
        .gap(3.0)
        .px(9.0)
        .pt(12.0)
        .child(
            navitem(
                "All threads",
                s.channels.iter().map(|c| c.threads).sum(),
                s.channel.is_none() && matches!(s.page, Page::Feed | Page::Thread(_)),
                "all-threads",
                icon(&glyphs::FILE_TEXT, 15.0).color(c(ACCENT)),
            )
            .on_press(|s: &mut State| s.select(None)),
        );
    for (name, key, page, glyph, count, selected) in [
        (
            "Activity",
            "activity",
            Page::Activity,
            &glyphs::EYE,
            s.desk.unread,
            s.page == Page::Activity,
        ),
        (
            "Tasks",
            "tasks",
            Page::Tasks,
            &glyphs::CHECK,
            -1,
            matches!(s.page, Page::Tasks | Page::Task(_) | Page::NewTask),
        ),
        (
            "Workspace",
            "workspace",
            Page::Workspace,
            &glyphs::FOLDER,
            -1,
            matches!(
                s.page,
                Page::Workspace
                    | Page::Schedules
                    | Page::Watchers
                    | Page::Schemas
                    | Page::Profiles
                    | Page::People
                    | Page::Access
                    | Page::Sources
                    | Page::Inbox
            ),
        ),
    ] {
        items = items.child(
            navitem(
                name,
                count,
                selected,
                key,
                icon(glyph, 15.0).color(c(MUTED)),
            )
            .on_press(move |s: &mut State| s.go(page)),
        );
    }
    items = items.child(mono("CHANNELS", MUTED).pt(15.0).pb(5.0).px(8.0));
    let mut scopes = BTreeMap::<String, i64>::new();
    for ch in &s.channels {
        scopes.insert(ch.name.clone(), ch.threads);
        for (end, _) in ch.name.match_indices('/') {
            *scopes.entry(ch.name[..=end].to_owned()).or_default() += ch.threads;
        }
    }
    for (channel, count) in scopes {
        let group = channel.ends_with('/');
        let parts = channel.trim_end_matches('/');
        let leaf = parts.rsplit('/').next().unwrap_or(parts);
        let name = if group {
            format!("{leaf}/")
        } else {
            leaf.to_owned()
        };
        let key = format!("nav-{}", channel.replace('/', ":"));
        items = items.child(
            navitem(
                &name,
                count,
                s.channel.as_ref() == Some(&channel)
                    && matches!(s.page, Page::Feed | Page::Thread(_)),
                &key,
                if group {
                    icon(&glyphs::FOLDER, 14.0).color(c(MUTED))
                } else {
                    label("#", 15.0, 500.0, MUTED)
                },
            )
            .pl(9.0 + 7.0 * parts.matches('/').count().min(6) as f32)
            .label(format!("Browse {channel}"))
            .help(&channel)
            .on_press(move |s: &mut State| s.select(Some(channel.clone()))),
        );
    }
    let db = local::database_path();
    let status = if s.connected() {
        ("Local database", "#44815b")
    } else {
        ("Database unavailable", ACCENT)
    };
    let footer = col()
        .w_full()
        .px(16.0)
        .py(13.0)
        .gap(8.0)
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(el().size(7.0, 7.0).radius(3.5).fill(c(status.1)))
                .child(label(status.0, 12.0, 600.0, INK)),
        )
        .child(
            mono(
                db.file_name().and_then(|x| x.to_str()).unwrap_or("SQLite"),
                MUTED,
            )
            .truncate()
            .help(db.display().to_string()),
        );
    let side = col()
        .key("channels")
        .h_full()
        .fill(c(PANEL))
        .border_sides([0.0, 1.0, 0.0, 0.0], c(EDGE))
        .child(top)
        .child(rule())
        .child(scroll("channel-scroll", items))
        .child(rule())
        .child(footer);
    if narrow { side.w_full() } else { side.w(215.0) }
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
        .map(|s| format!("# {s}"))
        .unwrap_or_else(|| "All threads".into())
}
fn header(s: &State, narrow: bool) -> El {
    let mut bar = row()
        .w_full()
        .h(48.0)
        .px(if narrow { 14.0 } else { 28.0 })
        .center_y()
        .gap(8.0);
    if s.browse.jump_open {
        return bar
            .child(
                text_input(&s.browse.jump, TextStyle::new(14.0, 500.0))
                    .key("jump-id")
                    .label("Message ID")
                    .placeholder("Message ID", c(MUTED))
                    .single_line()
                    .grow(1.0)
                    .min_w(0.0)
                    .h(32.0)
                    .padding(7.0)
                    .radius(4.0)
                    .border(1.0, c(EDGE))
                    .on_edit(|s: &mut State| &mut s.browse.jump)
                    .on_input_key(|s: &mut State, k, _| {
                        if k.key == Key::Enter {
                            s.jump_to_id();
                            true
                        } else {
                            false
                        }
                    }),
            )
            .child(
                tool("submit-jump", "Open message", &glyphs::ARROW_RIGHT)
                    .on_press(State::jump_to_id),
            )
            .child(
                tool("cancel-jump", "Cancel", &glyphs::CLOSE)
                    .on_press(|s: &mut State| s.browse.jump_open = false),
            );
    }
    if narrow {
        bar =
            bar.child(action("open-nav", "Channels", false).on_press(|s: &mut State| s.nav = true));
    }
    if s.page == Page::Feed {
        bar = bar.child(mono("WORKSPACE", MUTED));
    } else {
        bar = bar
            .child(
                click(
                    label(
                        workspace::name(s.history.last().copied().unwrap_or(Page::Feed), s),
                        13.0,
                        550.0,
                        MUTED,
                    )
                    .key("back-to-feed")
                    .truncate()
                    .min_w(0.0),
                    "Back",
                )
                .on_press(State::back),
            )
            .child(mono("/", MUTED))
            .child(mono(workspace::name(s.page, s), INK));
    }
    bar.child(el().grow(1.0))
        .child(
            tool("jump-message", "Go to message ID", &glyphs::CURSOR_TEXT)
                .disabled(!s.connected())
                .on_press_tx(State::jump_field),
        )
        .child(
            click(
                row()
                    .key("choose-profile")
                    .gap(7.0)
                    .center_y()
                    .help("Local profiles"),
                "Local profiles",
            )
            .child(avatar(&s.actor))
            .child(
                label(&s.actor, 12.0, 550.0, INK)
                    .max_w(if narrow { 65.0 } else { 110.0 })
                    .truncate(),
            )
            .on_press(|s: &mut State| s.go(Page::Profiles)),
        )
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
        .radius(5.0)
        .fill(c("#fafbf9"))
        .child(icon(&glyphs::SEARCH, 15.0).color(c(MUTED)))
        .child(
            text_input(&s.query, TextStyle::new(13.0, 450.0))
                .single_line()
                .key("search")
                .label("Search threads")
                .placeholder("Search threads", c(MUTED))
                .grow(1.0)
                .min_w(0.0)
                .on_edit(|s: &mut State| &mut s.query)
                .on_input_change(|s: &mut State, _| {
                    s.starts.clear();
                    s.refresh();
                })
                .on_input_key(|s: &mut State, k, tx| {
                    let Some(first) = s.threads.first() else {
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
                s.starts.clear();
                s.refresh();
            }),
        );
    }
    field
}
fn message_text(m: &Message) -> &str {
    if m.deleted {
        "Message deleted"
    } else if m.text.is_empty() {
        m.attachments
            .first()
            .map_or("Attachment", |a| a.name.as_str())
    } else {
        &m.text
    }
}
fn author(m: &Message) -> &str {
    m.author.as_ref().map_or(&m.actor, |a| &a.name)
}
fn stamp(ts: &str) -> String {
    ts.get(..16).unwrap_or(ts).replace('T', " ")
}
fn item(t: &ThreadSummary) -> El {
    let m = &t.message;
    let id = m.id;
    let meta = row()
        .w_full()
        .gap(7.0)
        .center_y()
        .child(
            label(author(m), 12.0, 600.0, INK)
                .max_w(140.0)
                .truncate()
                .help(&m.actor),
        )
        .child(
            mono(format!("#{}", m.channel), MUTED)
                .grow(1.0)
                .min_w(0.0)
                .truncate(),
        )
        .child(mono(format!("#{id}"), MUTED));
    let activity = t.last.as_ref().unwrap_or(m);
    let caption = if t.replies == 1 {
        "1 reply".to_string()
    } else {
        format!("{} replies", t.replies)
    };
    let mut foot = row()
        .w_full()
        .gap(12.0)
        .center_y()
        .child(label(
            caption,
            12.0,
            500.0,
            if t.replies > 0 { ACCENT } else { MUTED },
        ))
        .child(mono(stamp(&activity.created_at), MUTED));
    if let Some(t) = &m.task {
        foot = foot.child(mono(format!("Task: {}", t.state), "#35634d"));
    } else if let Some((k, v)) = m.labels.iter().next() {
        foot = foot.child(
            mono(format!("{k}={v}"), "#336748")
                .grow(1.0)
                .min_w(0.0)
                .truncate(),
        );
    }
    click(
        row()
            .key(format!("thread-{id}"))
            .w_full()
            .px(12.0)
            .py(14.0)
            .gap(12.0)
            .border_sides([0.0, 0.0, 1.0, 0.0], c(EDGE)),
        &format!("Open thread {id}"),
    )
    .child(avatar(author(m)))
    .child(
        col()
            .min_w(0.0)
            .grow(1.0)
            .gap(6.0)
            .child(meta)
            .child(
                label(message_text(m), 16.0, 550.0, INK)
                    .w_full()
                    .wrap_anywhere()
                    .max_lines(2),
            )
            .child(foot),
    )
    .on_press(move |s: &mut State| s.go(Page::Thread(id)))
    .on_key(move |s: &mut State, k, tx| {
        let Some(i) = s.threads.iter().position(|t| t.message.id == id) else {
            return false;
        };
        let next = match k.key {
            Key::Down => i.saturating_add(1),
            Key::Up if i > 0 => i - 1,
            Key::Up => {
                tx.focus("search", true);
                return true;
            }
            _ => return false,
        };
        if let Some(t) = s.threads.get(next) {
            let key = format!("thread-{}", t.message.id);
            tx.focus(&key, true);
            tx.scroll_into_view(key);
        }
        true
    })
}
fn feed(s: &State, inset: f32) -> El {
    let mut head = col()
        .w_full()
        .px(inset)
        .pt(19.0)
        .pb(17.0)
        .gap(16.0)
        .child(
            row()
                .w_full()
                .center_y()
                .gap(10.0)
                .child(
                    label(title_for(s), 25.0, 630.0, INK)
                        .grow(1.0)
                        .min_w(0.0)
                        .truncate(),
                )
                .child(
                    action("new-thread", "New thread", true)
                        .disabled(!s.connected())
                        .on_press_tx(compose),
                ),
        )
        .child(
            row()
                .w_full()
                .center_y()
                .gap(8.0)
                .child(search(s))
                .child(filters::button())
                .child(
                    Checkbox::new(s.tasks_only)
                        .label("Tasks only")
                        .disabled(!s.connected())
                        .on_toggle(|s: &mut State| {
                            s.tasks_only = !s.tasks_only;
                            s.starts.clear();
                            s.refresh();
                        })
                        .build(&Kit::neutral(false))
                        .key("tasks-only"),
                ),
        );
    if s.browse.has_filters() {
        head = head.child(filters::summary(s));
    }
    let mut list = col().w_full().px(inset);
    if !s.connected() {
        list = list.child(label("Board unavailable", 18.0, 600.0, INK).py(24.0));
    } else if s.threads.is_empty() {
        list = list.child(col().w_full().py(38.0).gap(7.0).child(label(
            if !s.starts.is_empty() {
                "No threads on this page"
            } else if s.tasks_only {
                "No matching tasks"
            } else if s.query.text.is_empty() && !s.browse.has_filters() {
                "No threads yet"
            } else {
                "No matching threads"
            },
            18.0,
            600.0,
            INK,
        )));
    } else {
        list = list.child(
            row()
                .w_full()
                .h(28.0)
                .px(12.0)
                .center_y()
                .fill(c(PANEL))
                .child(mono("RECENT ACTIVITY", MUTED)),
        );
        for t in &s.threads {
            list = list.child(item(t));
        }
    }
    let mut panel = col()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .child(head)
        .child(scroll("feed-scroll", list));
    if s.older.is_some() || !s.starts.is_empty() {
        let page = s.starts.len() + 1;
        panel = panel.child(rule()).child(
            row()
                .w_full()
                .px(inset)
                .h(42.0)
                .gap(5.0)
                .center_y()
                .child(mono(format!("Page {page}"), MUTED).grow(1.0))
                .child(
                    tool("newer-threads", "Newer threads", &glyphs::CHEVRON_UP)
                        .disabled(s.starts.is_empty())
                        .opacity(if s.starts.is_empty() { 0.4 } else { 1.0 })
                        .on_press_tx(|s: &mut State, tx| {
                            s.starts.pop();
                            s.refresh();
                            tx.scroll_to_y("feed-scroll", 0.0);
                        }),
                )
                .child(
                    tool("older-threads", "Older threads", &glyphs::CHEVRON_DOWN)
                        .disabled(s.older.is_none())
                        .opacity(if s.older.is_some() { 1.0 } else { 0.4 })
                        .on_press_tx(|s: &mut State, tx| {
                            if let Some(before) = s.older {
                                s.starts.push(before);
                                s.refresh();
                                tx.scroll_to_y("feed-scroll", 0.0);
                            }
                        }),
                ),
        );
    }
    panel
}

fn body(s: &State, reply: bool) -> El {
    text_input(&s.body, TextStyle::new(15.0, 450.0).lh(21.0))
        .key("message-body")
        .label(if reply { "Reply" } else { "Message" })
        .placeholder(
            if reply {
                "Write a reply..."
            } else {
                "Write a message..."
            },
            c(MUTED),
        )
        .on_edit(|s: &mut State| &mut s.body)
        .on_input_paste(|s: &mut State, content, _| s.paste_file(content))
        .on_input_change(move |s: &mut State, _| {
            s.draft_changed();
            if reply {
                s.note_typing();
            }
        })
        .w_full()
        .h(if reply { 80.0 } else { 160.0 })
        .padding(12.0)
        .radius(5.0)
        .border(1.0, c(EDGE))
        .fill(c("#fafbf9"))
        .on_input_key(|s: &mut State, k, tx| {
            if k.key == Key::Enter && (k.mods.cmd || k.mods.ctrl) {
                s.send(tx);
                true
            } else if k.key == Key::Escape {
                s.back();
                true
            } else {
                false
            }
        })
}
fn send(s: &State, name: &str) -> El {
    let empty = s.body.text.trim().is_empty() && s.files.is_empty();
    action(
        "send",
        if s.delivery.later { "Schedule" } else { name },
        true,
    )
    .disabled(empty)
    .opacity(if empty { 0.55 } else { 1.0 })
    .on_press_tx(State::send)
}
fn attach(s: &State) -> El {
    tool("attach-file", "Attach file", &glyphs::FILE)
        .disabled(s.files.len() >= board_local::MAX_ATTACHMENTS)
        .on_press_tx(State::pick_file)
}
fn mention() -> El {
    click(
        el().key("mention-person")
            .size(30.0, 30.0)
            .center()
            .radius(5.0)
            .help("Mention a person or agent"),
        "Mention a person or agent",
    )
    .child(label("@", 17.0, 600.0, MUTED))
    .on_press_tx(State::pick_person)
}
fn draft(s: &State, reply: bool) -> El {
    let mut area = col()
        .w_full()
        .gap(6.0)
        .on_drop_files(State::drop_files)
        .child(body(s, reply));
    if !s.files.is_empty() {
        let mut list = col().w_full();
        for (i, (name, bytes)) in s.files.iter().enumerate() {
            list = list.child(
                row()
                    .w_full()
                    .center_y()
                    .gap(6.0)
                    .child(icon(&glyphs::FILE, 14.0).color(c(MUTED)))
                    .child(
                        label(format!("{name} ({} bytes)", bytes.len()), 12.0, 500.0, INK)
                            .grow(1.0)
                            .min_w(0.0)
                            .truncate(),
                    )
                    .child(
                        tool(
                            &format!("remove-file-{i}"),
                            "Remove attachment",
                            &glyphs::CLOSE,
                        )
                        .on_press(move |s: &mut State| {
                            if i < s.files.len() {
                                s.files.remove(i);
                                s.files_changed();
                            }
                        }),
                    ),
            );
        }
        area = area.child(
            col()
                .key("queued-files")
                .w_full()
                .max_h(86.0)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .child(list),
        );
    }
    area
}
fn composer(s: &State, inset: f32) -> El {
    let field = text_input(&s.destination, TextStyle::new(14.0, 450.0))
        .single_line()
        .key("destination")
        .label("Channel")
        .placeholder("general", c(MUTED))
        .w_full()
        .h(38.0)
        .padding(9.0)
        .radius(5.0)
        .border(1.0, c(EDGE))
        .on_edit(|s: &mut State| &mut s.destination)
        .on_input_change(|s: &mut State, _| s.draft_changed());
    let form = col()
        .w_full()
        .px(inset)
        .py(22.0)
        .gap(18.0)
        .child(label("New thread", 25.0, 630.0, INK))
        .child(
            col()
                .w_full()
                .gap(7.0)
                .child(label("Channel", 13.0, 600.0, INK))
                .child(field),
        )
        .child(
            col()
                .w_full()
                .gap(7.0)
                .child(label("Message", 13.0, 600.0, INK))
                .child(draft(s, false)),
        )
        .child(
            row()
                .w_full()
                .center_y()
                .gap(8.0)
                .child(attach(s))
                .child(mention())
                .child(delivery::options(s))
                .child(el().grow(1.0))
                .child(action("cancel", "Cancel", false).on_press(|s: &mut State| s.go(Page::Feed)))
                .child(send(s, "Post thread")),
        );
    scroll("compose-scroll", form)
}

fn task_details(t: &Task) -> El {
    fn detail(name: &str, value: &str) -> El {
        row()
            .w_full()
            .gap(10.0)
            .child(label(name, 12.0, 500.0, MUTED).w(68.0))
            .child(
                label(value, 13.0, 500.0, INK)
                    .grow(1.0)
                    .min_w(0.0)
                    .wrap_anywhere(),
            )
    }
    let mut panel = col()
        .key("task-details")
        .w_full()
        .gap(5.0)
        .py(12.0)
        .border_sides([0.0, 0.0, 1.0, 0.0], c(EDGE))
        .child(
            row()
                .w_full()
                .center_y()
                .gap(10.0)
                .child(mono("TASK", MUTED))
                .child(label(&t.state, 15.0, 600.0, ACCENT)),
        )
        .child(detail("Owner", &t.owner))
        .child(detail(
            "Assigned",
            t.assignee.as_deref().unwrap_or("Unassigned"),
        ));
    if let Some(ref holder) = t.holder {
        panel = panel.child(detail("Claimed by", holder));
    }
    if let Some(time) = t
        .lease_until
        .and_then(|s| chrono::DateTime::from_timestamp(s, 0))
    {
        panel = panel.child(detail(
            "Lease until",
            &time.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        ));
    }
    if !t.needs.is_empty() {
        let ids = t
            .needs
            .iter()
            .map(|id| format!("#{id}"))
            .collect::<Vec<_>>()
            .join(", ");
        panel = panel.child(detail("Needs", &ids));
    }
    if let Some(ref result) = t.result {
        panel = panel.child(detail("Result", result));
    }
    panel
}

fn message(s: &State, m: &Message) -> El {
    let id = m.id;
    let starred = m
        .reactions
        .get("star")
        .is_some_and(|who| who.contains(&s.actor));
    let star = tool(
        &format!("star-{id}"),
        if starred {
            "Remove star"
        } else {
            "Star message"
        },
        if starred {
            &glyphs::STAR_FILLED
        } else {
            &glyphs::STAR
        },
    )
    .disabled(m.deleted)
    .on_press(move |s: &mut State| s.star(id, starred));
    let mut content = col()
        .grow(1.0)
        .min_w(0.0)
        .gap(6.0)
        .child(
            row()
                .w_full()
                .center_y()
                .gap(5.0)
                .child(
                    label(author(m), 14.0, 600.0, INK)
                        .help(&m.actor)
                        .grow(1.0)
                        .min_w(0.0)
                        .truncate(),
                )
                .child(mono(format!("#{id}"), MUTED))
                .child(
                    tool(
                        &format!("reply-{id}"),
                        &format!("Reply to #{id}"),
                        &glyphs::ARROW_RIGHT,
                    )
                    .on_press_tx(move |s: &mut State, tx| {
                        s.reply_to = Some(id);
                        s.draft_changed();
                        tx.focus_when_shown("message-body", true);
                    }),
                )
                .child(star)
                .child(
                    tool(&format!("inspect-{id}"), "Message details", &glyphs::EYE).on_press_tx(
                        move |s: &mut State, tx| {
                            s.open_properties(id);
                            tx.focus_when_shown("close-properties", true);
                        },
                    ),
                ),
        )
        .child(
            mono(
                match m.reply_to {
                    Some(parent) => format!("{} UTC   Reply to #{parent}", stamp(&m.created_at)),
                    None => format!("{} UTC", stamp(&m.created_at)),
                },
                MUTED,
            )
            .w_full()
            .wrap_anywhere(),
        );
    if author(m) != m.actor {
        content = content.child(mono(&m.actor, MUTED).w_full().wrap_anywhere());
    }
    content = content.child(rich::body(s, m));
    for (i, l) in m.links.iter().enumerate().take(12) {
        let title = l.message.map_or_else(
            || "Quoted message".into(),
            |n| format!("Quoted message #{n}"),
        );
        let mut quote = col()
            .key(format!("quote-{id}-{i}"))
            .w_full()
            .gap(4.0)
            .pl(9.0)
            .border_sides([0.0, 0.0, 0.0, 2.0], c(EDGE))
            .child(mono(title, MUTED));
        if let Some(a) = &l.author {
            quote = quote.child(
                label(a.chars().take(120).collect::<String>(), 12.0, 600.0, INK)
                    .w_full()
                    .wrap_anywhere(),
            );
        }
        if let Some(t) = &l.text {
            quote = quote.child(
                label(t.chars().take(500).collect::<String>(), 13.0, 450.0, INK)
                    .w_full()
                    .wrap_anywhere()
                    .selectable(true)
                    .max_lines(8),
            );
        } else {
            quote = quote.child(mono(
                match l.reason.as_deref() {
                    Some("not_public") => "Not public",
                    Some("not_in_channel") => "Not in channel",
                    _ => "Quote unavailable",
                },
                MUTED,
            ));
        }
        content = content.child(quote);
    }
    if let Some(at) = m.edited_at.as_deref().filter(|_| !m.deleted) {
        content = content.child(mono(format!("Edited {} UTC", stamp(at)), MUTED));
    }
    if let Some(send) = &m.schedule {
        content = content.child(
            mono(format!("Schedule #{}   Send {}", send.id, send.n), MUTED)
                .w_full()
                .wrap_anywhere(),
        );
    }
    if let Some(origin) = &m.from {
        content = content.child(
            mono(format!("Source: {origin}"), MUTED)
                .w_full()
                .wrap_anywhere(),
        );
    }
    if m.no_sync {
        content = content.child(mono("Local only", MUTED));
    }
    if m.labels.keys().any(|k| k != "board.task") {
        let labels = m
            .labels
            .iter()
            .filter(|(k, _)| k.as_str() != "board.task")
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("   ");
        content = content.child(mono(labels, "#376345").w_full().wrap_anywhere());
    }
    if !m.attachments.is_empty() {
        for a in &m.attachments {
            let (file, name) = (a.id, a.name.clone());
            content = content.child(
                row()
                    .w_full()
                    .center_y()
                    .gap(6.0)
                    .child(icon(&glyphs::FILE, 14.0).color(c(MUTED)))
                    .child(
                        label(format!("{} ({} bytes)", a.name, a.size), 12.0, 500.0, MUTED)
                            .grow(1.0)
                            .min_w(0.0)
                            .wrap_anywhere(),
                    )
                    .child(
                        tool(
                            &format!("preview-{id}-{file}"),
                            "Preview attachment",
                            &glyphs::EYE,
                        )
                        .on_press_tx(move |s: &mut State, tx| s.open_attachment(id, file, tx)),
                    )
                    .child(
                        tool(
                            &format!("save-file-{file}"),
                            "Save attachment",
                            &glyphs::FOLDER_OPEN,
                        )
                        .on_press_tx(move |s: &mut State, tx| s.save_attachment(file, &name, tx)),
                    ),
            );
        }
    }
    if !m.reactions.is_empty() {
        content = content.child(
            mono(
                m.reactions
                    .iter()
                    .map(|(k, v)| format!("{k} {} ({})", v.len(), v.join(", ")))
                    .collect::<Vec<_>>()
                    .join("   "),
                ACCENT,
            )
            .w_full()
            .wrap_anywhere(),
        );
    }
    for (i, n) in m.file_notices.iter().enumerate() {
        content = content.child(
            mono(format!("File not copied: {}   {}", n.name, n.reason), MUTED)
                .key(format!("file-notice-{id}-{i}"))
                .w_full()
                .wrap_anywhere(),
        );
    }
    let mut entry = row()
        .key(format!("message-{id}"))
        .w_full()
        .gap(10.0)
        .py(14.0)
        .border_sides([0.0, 0.0, 1.0, 0.0], c(EDGE))
        .child(avatar(author(m)))
        .child(content);
    if s.browse.focused == Some(id) {
        entry = entry
            .fill(c("#f5f8ef"))
            .border_sides([0.0, 0.0, 1.0, 2.0], c("#65845f"));
    }
    entry
}
fn conversation(s: &State, id: i64, inset: f32) -> El {
    let mut messages = col().w_full().px(inset).pb(8.0);
    for m in &s.thread {
        messages = messages.child(message(s, m));
        if let Some(task) = &m.task {
            let task_id = task.id;
            messages = messages.child(task_details(task)).child(
                action("open-task", "Manage task", false)
                    .on_press(move |s: &mut State| s.go(Page::Task(task_id))),
            );
        }
    }
    let title = row()
        .w_full()
        .center_y()
        .px(inset)
        .pt(20.0)
        .pb(10.0)
        .gap(10.0)
        .child(label(format!("Thread #{id}"), 23.0, 630.0, INK))
        .child(
            mono(
                s.thread
                    .first()
                    .map(|m| format!("#{}", m.channel))
                    .unwrap_or_default(),
                MUTED,
            )
            .grow(1.0)
            .min_w(0.0)
            .truncate(),
        );
    let title = if s
        .thread
        .first()
        .is_some_and(|m| m.task.is_none() && !m.deleted)
    {
        title.child(
            tool("make-task", "Make this thread a task", &glyphs::CHECK)
                .on_press(move |s: &mut State| s.create_task(Some(id))),
        )
    } else {
        title
    };
    let mut content = col().w_full().grow(1.0).min_h(0.0).child(title);
    if s.browse.older || s.browse.newer || s.browse.position != Position::Latest {
        let first = s.thread.get(1).map_or(id, |m| m.id);
        let last = s.thread.last().map_or(id, |m| m.id);
        let mut paging = row()
            .w_full()
            .center_y()
            .gap(4.0)
            .px(inset)
            .child(
                mono(format!("Replies #{first} to #{last}"), MUTED)
                    .grow(1.0)
                    .min_w(0.0)
                    .truncate(),
            )
            .child(
                tool("older-replies", "Older replies", &glyphs::CHEVRON_UP)
                    .disabled(!s.browse.older)
                    .on_press_tx(move |s: &mut State, tx| {
                        s.page_replies(Position::Before(first), tx)
                    }),
            )
            .child(
                tool("newer-replies", "Newer replies", &glyphs::CHEVRON_DOWN)
                    .disabled(!s.browse.newer)
                    .on_press_tx(move |s: &mut State, tx| {
                        s.page_replies(Position::After(last), tx)
                    }),
            );
        if s.browse.position != Position::Latest {
            paging = paging.child(
                action("latest-replies", "Latest", false)
                    .on_press_tx(|s: &mut State, tx| s.page_replies(Position::Latest, tx)),
            );
        }
        content = content.child(paging);
    }
    for pending in &s.browse.schedules {
        let sid = pending.id;
        let mut pending_row = row().w_full().center_y().gap(5.0).child(
            mono(
                format!(
                    "Scheduled reply #{}   {}",
                    pending.id,
                    workspace::utc(pending.due)
                ),
                MUTED,
            )
            .key(format!("scheduled-reply-{}", pending.id))
            .py(6.0)
            .grow(1.0)
            .min_w(0.0)
            .wrap_anywhere(),
        );
        if pending.actor == s.actor
            || pending
                .actor
                .strip_prefix(&s.actor)
                .is_some_and(|v| v.starts_with('/'))
        {
            pending_row = if s.browse.cancel == Some(sid) {
                pending_row
                    .child(
                        action(&format!("confirm-pending-{sid}"), "Cancel send", false)
                            .on_press(move |s: &mut State| s.cancel_pending(sid)),
                    )
                    .child(
                        tool(
                            &format!("keep-pending-{sid}"),
                            "Keep schedule",
                            &glyphs::CLOSE,
                        )
                        .on_press(|s: &mut State| s.browse.cancel = None),
                    )
            } else {
                pending_row.child(
                    tool(
                        &format!("cancel-pending-{sid}"),
                        "Cancel scheduled reply",
                        &glyphs::CLOSE,
                    )
                    .on_press(move |s: &mut State| s.browse.cancel = Some(sid)),
                )
            };
        }
        messages = messages.child(pending_row);
    }
    let mut actions = row()
        .w_full()
        .gap(6.0)
        .center_y()
        .child(attach(s))
        .child(mention())
        .child(delivery::options(s));
    if let Some(target) = s.reply_to {
        actions = actions
            .child(
                mono(format!("Replying to #{target}"), ACCENT)
                    .min_w(0.0)
                    .truncate(),
            )
            .child(
                tool("clear-reply-to", "Reply to thread instead", &glyphs::CLOSE).on_press(
                    |s: &mut State| {
                        s.reply_to = None;
                        s.draft_changed();
                    },
                ),
            );
    }
    actions = actions.child(el().grow(1.0)).child(send(s, "Reply"));
    let mut compose = col().w_full().px(inset).py(12.0).gap(8.0);
    if let Some(id) = s.delivery.after {
        compose = compose.child(delivery::guard(id));
    }
    if !s.typing.is_empty() {
        compose = compose.child(
            mono(
                s.typing
                    .iter()
                    .map(|t| format!("{}: {}", t.actor, t.status))
                    .collect::<Vec<_>>()
                    .join("   "),
                MUTED,
            )
            .key("typing")
            .w_full()
            .truncate(),
        );
    }
    content
        .child(scroll("conversation", messages))
        .child(rule())
        .child(compose.child(draft(s, true)).child(actions))
}
pub fn view(s: &State, cx: &mut Cx<State>) -> El {
    cx.foreground(c(INK));
    cx.focus_ring(2.0, 1.0, c(ACCENT));
    let narrow = cx
        .layout_with("board-window", |r| r[2] < 690.0)
        .unwrap_or(false);
    let inset = if narrow { 16.0 } else { 30.0 };
    let mut layout = row()
        .key("board-window")
        .w_full()
        .h_full()
        .fill(Color::WHITE);
    if !narrow || s.nav {
        layout = layout.child(sidebar(s, narrow));
    }
    if !narrow || !s.nav {
        let mut pane = col()
            .grow(1.0)
            .min_w(0.0)
            .h_full()
            .child(header(s, narrow))
            .child(rule());
        if let Some(e) = &s.error {
            pane = pane.child(
                row()
                    .w_full()
                    .px(inset)
                    .py(8.0)
                    .center_y()
                    .gap(8.0)
                    .fill(c(TINT))
                    .child(
                        label(e, 13.0, 500.0, ACCENT)
                            .grow(1.0)
                            .min_w(0.0)
                            .wrap_anywhere(),
                    )
                    .child(if s.connected() {
                        tool("dismiss-error", "Dismiss error", &glyphs::CLOSE)
                            .on_press(|s: &mut State| s.error = None)
                    } else {
                        action("retry-open", "Retry", false).on_press(State::retry)
                    }),
            );
        }
        let page = if s.access_blocked {
            label("View unavailable", 16.0, 550.0, MUTED)
                .px(inset)
                .py(18.0)
        } else if s.delivery.open && matches!(s.page, Page::Compose | Page::Thread(_)) {
            delivery::view(s, inset)
        } else if s.people.picking || s.page == Page::People {
            people::view(s, inset)
        } else if s.browse.filter_open {
            filters::view(s, inset)
        } else {
            match s.page {
                Page::Feed => feed(s, inset),
                Page::Compose => composer(s, inset),
                Page::Thread(_) if let Some(p) = &s.preview => preview::view(p, inset),
                Page::Thread(id) => match s
                    .properties
                    .as_ref()
                    .and_then(|p| s.thread.iter().find(|m| m.id == p.id).map(|m| (p, m)))
                {
                    Some((p, m)) => properties::view(s, p, m, inset),
                    None => conversation(s, id, inset),
                },
                _ => workspace::view(s, inset),
            }
        };
        layout = layout.child(pane.child(page));
    }
    layout
}
