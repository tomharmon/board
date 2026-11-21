mod admin;
mod sources;
mod tasks;

use super::*;
use crate::state::{ActivityMode, TaskCommand};
use rune::TextEdit;
use rune_kit::SegmentedControl;

pub(super) fn name(page: Page, s: &State) -> String {
    match page {
        Page::Feed => title_for(s),
        Page::Thread(id) => format!("#{id}"),
        Page::Compose => "New thread".into(),
        Page::Activity => "Activity".into(),
        Page::Tasks => "Tasks".into(),
        Page::Task(id) => format!("Task #{id}"),
        Page::NewTask => "New task".into(),
        Page::Workspace => "Workspace".into(),
        Page::Schedules => "Schedules".into(),
        Page::Watchers => "Watchers".into(),
        Page::Schemas => "Schemas".into(),
        Page::Access => "Access".into(),
        Page::Sources => "Sources".into(),
        Page::Inbox => "Inbox".into(),
        Page::Profiles => "Profiles".into(),
        Page::People => "People".into(),
    }
}

fn seg(labels: &[&str], selected: usize, name: &str, key: &str, pick: fn(&mut State, usize)) -> El {
    SegmentedControl::new(labels, Some(selected))
        .label(name)
        .fit()
        .on_pick(move |s: &mut State, i, _| pick(s, i))
        .build(&Kit::neutral(false))
        .key(key)
}

pub(super) fn field(
    key: &str,
    name: &str,
    value: &TextEdit,
    write: fn(&mut State) -> &mut TextEdit,
    multi: bool,
) -> El {
    let mut input = text_input(value, TextStyle::new(14.0, 450.0))
        .key(key)
        .label(name)
        .on_edit(write)
        .w_full()
        .padding(9.0)
        .radius(5.0)
        .border(1.0, c(EDGE))
        .fill(c("#fafbf9"));
    if multi {
        input = input.h(84.0);
    } else {
        input = input.single_line().h(36.0);
    }
    col()
        .w_full()
        .gap(5.0)
        .child(label(name, 13.0, 580.0, INK))
        .child(input)
}

fn heading(name: &str, extra: Option<El>) -> El {
    let bar = row().w_full().gap(8.0).center_y().child(
        label(name, 22.0, 630.0, INK)
            .grow(1.0)
            .min_w(0.0)
            .wrap_anywhere(),
    );
    match extra {
        Some(button) => bar.child(button),
        None => bar,
    }
}
fn empty(name: &str) -> El {
    label(name, 15.0, 500.0, MUTED)
        .w_full()
        .py(12.0)
        .wrap_anywhere()
}
fn listrow(key: &str, title: &str, detail: &str, open: impl Fn(&mut State) + 'static) -> El {
    click(
        row()
            .key(key)
            .w_full()
            .py(11.0)
            .px(3.0)
            .center_y()
            .gap(8.0)
            .border_sides([0.0, 0.0, 1.0, 0.0], c(EDGE)),
        title,
    )
    .child(
        col()
            .grow(1.0)
            .min_w(0.0)
            .gap(5.0)
            .child(
                label(title, 15.0, 580.0, INK)
                    .w_full()
                    .wrap_anywhere()
                    .max_lines(3),
            )
            .child(mono(detail, MUTED).w_full().wrap_anywhere()),
    )
    .child(icon(&glyphs::CARET_RIGHT, 14.0).color(c(MUTED)))
    .on_press(open)
}
fn pager(s: &State) -> El {
    let watcher = s.page == Page::Watchers;
    let prev = if watcher {
        !s.desk.watcher_pages.is_empty()
    } else {
        !s.desk.pages.is_empty()
    };
    let next = if watcher {
        s.desk.watcher_next.is_some()
    } else {
        s.desk.next.is_some()
    };
    if !prev && !next {
        return el().h(0.0);
    }
    row()
        .w_full()
        .gap(8.0)
        .center_y()
        .py(4.0)
        .child(
            mono(
                format!(
                    "Page {}",
                    1 + if watcher {
                        s.desk.watcher_pages.len()
                    } else {
                        s.desk.pages.len()
                    }
                ),
                MUTED,
            )
            .grow(1.0),
        )
        .child(
            tool("previous-page", "Previous page", &glyphs::CHEVRON_UP)
                .disabled(!prev)
                .on_press_tx(move |s: &mut State, tx| {
                    if watcher {
                        s.desk.watcher_pages.pop();
                    } else {
                        s.desk.pages.pop();
                    }
                    s.refresh();
                    tx.scroll_to_y("work-scroll", 0.0);
                }),
        )
        .child(
            tool("next-page", "Next page", &glyphs::CHEVRON_DOWN)
                .disabled(!next)
                .on_press_tx(move |s: &mut State, tx| {
                    if watcher {
                        if let Some(n) = s.desk.watcher_next.clone() {
                            s.desk.watcher_pages.push(n);
                        }
                    } else if let Some(n) = s.desk.next {
                        s.desk.pages.push(n);
                    }
                    s.refresh();
                    tx.scroll_to_y("work-scroll", 0.0);
                }),
        )
}
pub(super) fn utc(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0).map_or_else(
        || ts.to_string(),
        |t| t.format("%Y-%m-%d %H:%M UTC").to_string(),
    )
}

fn activity(s: &State) -> El {
    let d = &s.desk;
    let mut panel = col()
        .w_full()
        .gap(12.0)
        .child(heading(
            "Activity",
            Some(mono(format!("{} unread", d.unread), ACCENT)),
        ))
        .child(seg(
            &["Messages", "Changes", "Unread"],
            match d.mode {
                ActivityMode::Messages => 0,
                ActivityMode::Changes => 1,
                ActivityMode::Unread => 2,
            },
            "Activity view",
            "activity-modes",
            |s, i| {
                s.desk.mode = [
                    ActivityMode::Messages,
                    ActivityMode::Changes,
                    ActivityMode::Unread,
                ][i];
                s.desk.first_page();
                s.refresh();
            },
        ));
    if d.mode == ActivityMode::Messages {
        panel = panel.child(
            row()
                .w_full()
                .center_y()
                .gap(8.0)
                .child(filters::summary(s).grow(1.0).min_w(0.0))
                .child(filters::button()),
        );
        if d.messages.is_empty() {
            panel = panel.child(empty("No messages yet"));
        }
        for m in &d.messages {
            let id = m.id;
            panel = panel.child(
                col()
                    .key(format!("activity-message-{}", m.id))
                    .w_full()
                    .gap(5.0)
                    .pb(10.0)
                    .border_sides([0.0, 0.0, 1.0, 0.0], c(EDGE))
                    .child(
                        row()
                            .w_full()
                            .center_y()
                            .gap(8.0)
                            .child(
                                label(author(m), 14.0, 600.0, INK)
                                    .max_w(130.0)
                                    .truncate()
                                    .help(&m.actor),
                            )
                            .child(
                                mono(format!("#{}  #{}", m.channel, m.id), MUTED)
                                    .grow(1.0)
                                    .min_w(0.0)
                                    .truncate(),
                            )
                            .child(
                                tool(
                                    &format!("activity-open-{}", m.id),
                                    "Open thread",
                                    &glyphs::ARROW_RIGHT,
                                )
                                .on_press(move |s: &mut State| s.open_message_thread(id)),
                            ),
                    )
                    .child(rich::body(s, m))
                    .child(mono(stamp(&m.created_at), MUTED)),
            );
        }
    } else {
        let through = d.through;
        let mut controls = row().w_full().gap(6.0).center_y();
        if d.mode == ActivityMode::Changes {
            let latest = d.pages.is_empty();
            controls = controls.child(
                action(
                    "activity-range",
                    if latest { "From start" } else { "Latest" },
                    false,
                )
                .on_press(move |s: &mut State| {
                    s.desk.first_page();
                    if latest {
                        s.desk.pages.push(0);
                    }
                    s.refresh();
                }),
            );
        }
        controls = controls.child(el().grow(1.0)).child(
            action("mark-read", "Mark read", false)
                .help(format!(
                    "Mark all changes through event {through} read for {}",
                    s.actor
                ))
                .disabled(through <= d.seen)
                .on_press(move |s: &mut State| s.mark_seen(through)),
        );
        panel = panel.child(controls);
        if d.events.is_empty() {
            panel = panel.child(empty(if d.mode == ActivityMode::Unread {
                "No unread changes on this page"
            } else {
                "No changes yet"
            }));
        }
        for e in &d.events {
            let mut entry = col()
                .key(format!("event-{}", e.cursor))
                .w_full()
                .gap(4.0)
                .pb(8.0)
                .border_sides([0.0, 0.0, 1.0, 0.0], c(EDGE))
                .child(
                    row()
                        .w_full()
                        .gap(7.0)
                        .center_y()
                        .child(el().size(6.0, 6.0).radius(3.0).fill(
                            if e.cursor > d.seen && e.actor != s.actor {
                                c(ACCENT)
                            } else {
                                c(EDGE)
                            },
                        ))
                        .child(
                            label(&e.kind, 14.0, 600.0, INK)
                                .grow(1.0)
                                .min_w(0.0)
                                .wrap_anywhere(),
                        )
                        .child(mono(format!("@{}", e.cursor), MUTED)),
                )
                .child(
                    mono(
                        format!("{}    {} UTC", e.actor, stamp(&e.created_at)),
                        MUTED,
                    )
                    .w_full()
                    .wrap_anywhere(),
                );
            if let Some(m) = &e.message {
                let id = m.id;
                entry = entry.child(listrow(
                    &format!("open-event-{}", e.cursor),
                    message_text(m),
                    &format!("#{}  #{}", m.channel, m.id),
                    move |s| s.open_message_thread(id),
                ));
            } else if e.payload.as_object().is_some_and(|o| !o.is_empty()) {
                entry = entry.child(
                    mono(e.payload.to_string(), MUTED)
                        .w_full()
                        .wrap_anywhere()
                        .max_lines(3),
                );
            }
            panel = panel.child(entry);
        }
    }
    panel
}

pub(super) fn view(s: &State, inset: f32) -> El {
    let inner = match s.page {
        Page::Activity => activity(s),
        Page::Sources | Page::Inbox => sources::view(s),
        Page::Tasks | Page::Task(_) | Page::NewTask => tasks::view(s),
        _ => admin::view(s),
    };
    let paged = matches!(
        s.page,
        Page::Activity | Page::Tasks | Page::Schedules | Page::Watchers | Page::Inbox
    );
    let mut panel = col().w_full().grow(1.0).min_h(0.0).child(scroll(
        "work-scroll",
        col().w_full().px(inset).py(17.0).child(inner),
    ));
    if paged {
        panel = panel.child(pager(s).px(inset));
    }
    panel
}
