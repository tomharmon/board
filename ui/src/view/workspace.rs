mod admin;
mod sources;
mod tasks;

use super::*;
use crate::state::{ActivityMode, TaskCommand};
use rune::TextEdit;

fn seg(labels: &[&str], selected: usize, name: &str, key: &str, pick: fn(&mut State, usize)) -> El {
    let mut strip = row()
        .key(key)
        .label(name)
        .w_full()
        .center_y()
        .gap(17.0)
        .flex_wrap()
        .focus_group();
    for (i, title) in labels.iter().enumerate() {
        // Keep the original four task selectors stable for keyboard and scripted clients.
        let child_key = match (key, *title) {
            ("task-filter", "open") => "segment0".into(),
            ("task-filter", "ready") => "segment1".into(),
            ("task-filter", "claimed") => "segment2".into(),
            ("task-filter", "all") => "segment3".into(),
            ("task-filter", state) => format!("task-state-{state}"),
            _ => format!("segment{i}"),
        };
        strip = strip
            .child(tab(&child_key, title, i == selected).on_press(move |s: &mut State| pick(s, i)));
    }
    strip
}
pub(super) fn tab(key: &str, title: &str, active: bool) -> El {
    click(
        row().key(key).h(34.0).center_y().min_w(0.0).border_sides(
            [0.0, 0.0, 1.0, 0.0],
            if active { c(INK) } else { Color::TRANSPARENT },
        ),
        title,
    )
    .child(label(
        title.to_lowercase(),
        13.0,
        400.0,
        if active { INK } else { MUTED },
    ))
}
fn settings(page: Page) -> bool {
    matches!(
        page,
        Page::Workspace
            | Page::Access
            | Page::Roles
            | Page::Schemas
            | Page::Schedules
            | Page::Watchers
            | Page::Sources
            | Page::Inbox
            | Page::Profiles
            | Page::Activity
    )
}
fn settings_tabs(s: &State, inset: f32) -> El {
    let mut strip = row()
        .key("work-sections")
        .label("Settings sections")
        .w_full()
        .flex_wrap()
        .center_y()
        .gap(14.0)
        .px(inset)
        .focus_group()
        .border_sides([0.0, 0.0, 1.0, 0.0], c(EDGE));
    for (key, title, page) in [
        ("work-access", "access", Page::Access),
        ("work-schemas", "schemas", Page::Schemas),
        ("work-schedules", "schedules", Page::Schedules),
        ("work-watchers", "watchers", Page::Watchers),
        ("work-sources", "sync", Page::Sources),
        ("work-profiles", "profiles", Page::Profiles),
        ("work-roles", "agent roles", Page::Roles),
        ("work-history", "history", Page::Activity),
    ] {
        let active = s.page == page
            || (page == Page::Access && s.page == Page::Workspace)
            || (page == Page::Sources && s.page == Page::Inbox);
        strip = strip.child(tab(key, title, active).on_press(move |s: &mut State| s.go(page)));
    }
    if let Some(add) = admin::add_resource(s) {
        strip = strip.child(el().grow(1.0)).child(add);
    }
    strip
}

pub(super) fn field(
    key: &str,
    name: &str,
    value: &TextEdit,
    write: fn(&mut State) -> &mut TextEdit,
    multi: bool,
) -> El {
    let mut input = text_input(value, font(13.0, 400.0).lh(18.))
        .key(key)
        .label(name)
        .on_edit(write)
        .w_full()
        .color(c(INK))
        .padding(6.0)
        .px(8.)
        .border(1., c(if value.focused { DIM } else { CONTROL }))
        .radius(4.)
        .fill(c(TINT));
    if multi {
        input = input.h(72.0);
    } else {
        input = input.single_line().h(32.0);
    }
    col()
        .w_full()
        .gap(3.0)
        .child(label(name, 12.0, 500.0, MUTED))
        .child(input)
}

fn heading(name: &str, extra: Option<El>) -> El {
    let bar = row().w_full().gap(8.0).center_y().child(
        label(name, 12.0, 500.0, DIM)
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
    label(name, 12.0, 400.0, FAINT)
        .w_full()
        .py(12.0)
        .wrap_anywhere()
}
fn listrow(key: &str, title: &str, detail: &str, open: impl Fn(&mut State) + 'static) -> El {
    click(
        row().key(key).w_full().py(5.0).px(3.0).center_y().gap(10.0),
        title,
    )
    .child(
        row()
            .grow(1.0)
            .min_w(0.0)
            .center_y()
            .gap(14.0)
            .child(
                label(title, 13.0, 450.0, INK)
                    .grow(1.0)
                    .min_w(0.0)
                    .wrap_anywhere()
                    .max_lines(3),
            )
            .child(mono(detail, MUTED).grow(1.0).min_w(0.0).wrap_anywhere()),
    )
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

pub(super) fn task_list_for(s: &State, inset: f32, compact: bool) -> El {
    let mut panel = col().w_full().grow(1.0).min_h(0.0).child(scroll(
        "work-scroll",
        col()
            .w_full()
            .px(inset)
            .py(4.0)
            .child(tasks::list(s, compact)),
    ));
    if !s.desk.filtering {
        panel = panel
            .child(pager(s).px(inset))
            .child(tasks::composer(s, inset));
    }
    panel
}
pub(super) fn task_detail(s: &State, inset: f32) -> El {
    scroll(
        "task-detail-scroll",
        col().w_full().px(inset).py(12.0).child(tasks::detail(s)),
    )
}
pub(super) fn task_banner(s: &State, inset: f32) -> El {
    col()
        .key("task-detail-scroll")
        .w_full()
        .px(inset)
        .pt(15.0)
        .pb(13.0)
        .border_sides([0.0, 0.0, 1.0, 0.0], c(EDGE))
        .child(tasks::banner(s))
}
pub(super) fn view(s: &State, inset: f32) -> El {
    let inner = match s.page {
        Page::Activity => activity(s),
        Page::Roles => roles::view(s),
        Page::Sources | Page::Inbox => sources::view(s),
        Page::Tasks => return task_list_for(s, inset, false),
        Page::Task(_) => return task_detail(s, inset),
        Page::NewTask => tasks::new_task(s),
        _ => admin::view(s),
    };
    let paged = matches!(
        s.page,
        Page::Activity | Page::Schedules | Page::Watchers | Page::Inbox
    );
    let mut panel = col().w_full().grow(1.0).min_h(0.0);
    if settings(s.page) {
        panel = panel.child(settings_tabs(s, inset))
    }
    panel = panel.child(scroll(
        "work-scroll",
        col().w_full().px(inset).py(12.0).child(inner),
    ));
    if paged {
        panel = panel.child(pager(s).px(inset));
    }
    panel
}
