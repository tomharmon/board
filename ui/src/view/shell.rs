use super::*;
use std::collections::BTreeSet;

const HEADER: f32 = 46.0;
const BREAKPOINT: f32 = 900.0;

fn remember(s: &mut State) {
    if let Err(e) = s.layout.save() {
        s.error = Some(format!("Could not remember the window layout: {e}"));
    }
}

fn chrome(key: &str) -> El {
    row()
        .key(key)
        .w_full()
        .h(HEADER)
        .center_y()
        .gap(10.0)
        .pl(20.0)
        .pr(12.0)
        .border_sides([0.0, 0.0, 1.0, 0.0], c(EDGE))
}

fn link(key: &str, name: &str, count: Option<i64>, selected: bool, unread: bool) -> El {
    let mut base = row().key(key).w_full().h(30.0).px(8.0).gap(6.0).center_y();
    if selected {
        base = base.fill(c(TINT));
    }
    let mut link = click(base, name).child(
        label(
            name,
            13.0,
            if unread { 600.0 } else { 400.0 },
            if selected || unread { INK } else { MUTED },
        )
        .grow(1.0)
        .min_w(0.0)
        .truncate(),
    );
    if unread {
        link = link.child(
            el().key(format!("{key}-unread"))
                .size(6.0, 6.0)
                .radius(3.0)
                .fill(c(INK))
                .label("Unread messages"),
        );
    } else if let Some(n) = count {
        link = link.child(mono(n.to_string(), FAINT));
    }
    link
}

fn sidebar(s: &State, drawer: bool, width: f32) -> El {
    let path = local::database_path();
    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("board");
    let brand = chrome("sidebar-header")
        .pl(16.0)
        .pr(8.0)
        .child(
            label(filename, 13.0, 600.0, INK)
                .grow(1.0)
                .min_w(0.0)
                .truncate()
                .help(path.display().to_string()),
        )
        .child(
            tool("close-nav", "Hide the sidebar", &icons::PANEL_LEFT_CLOSE).on_press_tx(
                move |s: &mut State, tx| {
                    if drawer {
                        s.nav = false;
                    } else {
                        s.layout.collapsed = true;
                        remember(s);
                    }
                    tx.focus_when_shown("open-nav", true);
                },
            ),
        );
    let browsing = s.page.is_feed() || matches!(s.page, Page::Thread(_));
    let mut items = col()
        .w_full()
        .role("list")
        .focus_group()
        .px(8.0)
        .pt(8.0)
        .pb(12.0)
        .child(
            link(
                "all-threads",
                "activity",
                None,
                s.channel.is_none() && browsing,
                false,
            )
            .on_press(|s: &mut State| s.select(None)),
        )
        .child(
            link(
                "tasks",
                "tasks",
                (s.open_tasks > 0).then_some(s.open_tasks),
                matches!(s.page, Page::Tasks | Page::Task(_) | Page::NewTask),
                false,
            )
            .on_press(|s: &mut State| s.go(Page::Tasks)),
        )
        .child(
            link(
                "swarms",
                "swarms",
                (s.active_swarms > 0).then_some(s.active_swarms),
                matches!(s.page, Page::Swarms | Page::Swarm(_)),
                false,
            )
            .on_press(|s: &mut State| s.go(Page::Swarms)),
        )
        .child(
            row()
                .w_full()
                .mt(12.0)
                .pl(8.0)
                .center_y()
                .child(mono("channels", FAINT).grow(1.0))
                .child(
                    tool("nav-new", "New channel", &icons::PLUS)
                        .size(24.0, 24.0)
                        .on_press_tx(|s: &mut State, tx| {
                            s.browse.channel_name.clear();
                            s.browse.adding_channel = true;
                            s.browse.private_channel = false;
                            s.refresh();
                            tx.focus_when_shown("new-channel-name", true);
                        }),
                ),
        );
    if s.browse.adding_channel {
        items = items.child(
            row()
                .w_full()
                .center_y()
                .child(
                    text_input(&s.browse.channel_name, font(12.0, 400.0))
                        .key("new-channel-name")
                        .label("New channel name")
                        .single_line()
                        .placeholder("channel name", c(FAINT))
                        .grow(1.0)
                        .min_w(0.0)
                        .h(28.0)
                        .px(8.0)
                        .fill(c(TINT))
                        .on_edit(|s: &mut State| &mut s.browse.channel_name)
                        .on_input_key(|s: &mut State, k, _| match k.key {
                            Key::Enter => {
                                s.add_channel();
                                true
                            }
                            Key::Escape => {
                                s.browse.adding_channel = false;
                                true
                            }
                            _ => false,
                        }),
                )
                .child(
                    tool("submit-channel", "Create channel", &glyphs::CHECK)
                        .on_press(State::add_channel),
                ),
        );
        if s.desk.access.registry() {
            items = items.child(
                rune_kit::Checkbox::new(s.browse.private_channel)
                    .label("Private channel")
                    .on_toggle(|s: &mut State| s.browse.private_channel = !s.browse.private_channel)
                    .build(&Kit::neutral(dark()))
                    .key("new-channel-private"),
            );
            if s.browse.private_channel {
                items = items.child(
                    label(
                        "Restricted to you and your profiles through credentials.",
                        11.,
                        400.,
                        MUTED,
                    )
                    .w_full()
                    .px(8.)
                    .wrap_anywhere(),
                );
                items = items.child(
                    label(
                        "Anyone with direct access to the database can still read it.",
                        11.,
                        400.,
                        MUTED,
                    )
                    .w_full()
                    .px(8.)
                    .wrap_anywhere(),
                );
            }
        }
    }
    let mut groups = BTreeSet::from([String::new()]);
    for channel in &s.channels {
        for (end, _) in channel.name.match_indices('/') {
            groups.insert(channel.name[..end].to_owned());
        }
    }
    for group in groups {
        if !group.is_empty() {
            let scope = format!("{group}/");
            items = items.child(
                click(
                    mono(&group, FAINT)
                        .key(format!("nav-{}", scope.replace('/', ":")))
                        .w_full()
                        .mt(12.0)
                        .h(24.0)
                        .px(8.0)
                        .truncate(),
                    &format!("Browse {scope}"),
                )
                .help(&scope)
                .on_press(move |s: &mut State| s.select(Some(scope.clone()))),
            );
        }
        for ch in &s.channels {
            let (parent, leaf) = ch.name.rsplit_once('/').unwrap_or(("", &ch.name));
            if parent != group {
                continue;
            }
            let channel = ch.name.clone();
            items = items.child(
                link(
                    &format!("nav-{}", channel.replace('/', ":")),
                    &format!("#{leaf}"),
                    Some(ch.threads),
                    browsing && s.channel.as_ref() == Some(&channel),
                    s.unread_channels.contains(&channel),
                )
                .label(format!("Browse {channel}"))
                .help(&channel)
                .on_press(move |s: &mut State| s.select(Some(channel.clone()))),
            );
        }
    }
    let footer = col()
        .w_full()
        .px(8.0)
        .pt(4.0)
        .pb(12.0)
        .child(
            click(
                row().key("workspace").h(30.0).px(8.0).center_y().gap(8.0),
                "Settings",
            )
            .child(icon(&icons::SETTINGS, 14.0).color(c(FAINT)))
            .child(label("settings", 12.0, 400.0, FAINT))
            .on_press(|s: &mut State| s.go(Page::Workspace)),
        )
        .child(
            click(
                row()
                    .key("choose-profile")
                    .h(36.0)
                    .px(8.0)
                    .center_y()
                    .gap(6.0),
                "Choose profile",
            )
            .child(avatar_size(&s.actor, 24.0))
            .child(
                mono(format!("as {}", s.actor), MUTED)
                    .grow(1.0)
                    .min_w(0.0)
                    .truncate()
                    .help(&s.actor),
            )
            .child(icon(&icons::CHEVRONS_UP_DOWN, 13.0).color(c(MUTED)))
            .on_press(|s: &mut State| s.go(Page::Profiles)),
        );
    col()
        .key("channels")
        .role("navigation")
        .label("Board navigation")
        .w(width)
        .h_full()
        .fill(c(BG))
        .border_sides([0.0, 1.0, 0.0, 0.0], c(EDGE))
        .child(brand)
        .child(scroll("channel-scroll", items))
        .child(footer)
}

fn resize(sidebar: bool, width: f32, limit: f32) -> El {
    let key = if sidebar {
        "sidebar-resize"
    } else {
        "thread-resize"
    };
    let name = if sidebar {
        "Resize the sidebar"
    } else {
        "Resize the thread"
    };
    let apply = move |s: &mut State, n: f32| {
        if sidebar {
            s.layout.sidebar = n.round().clamp(180.0, 400.0);
        } else {
            s.layout.thread = n.round().clamp(320.0, limit);
        }
    };
    let direction = if sidebar { 1.0 } else { -1.0 };
    el().key(key)
        .w(7.0)
        .h_full()
        .role("separator")
        .label(name)
        .help(name)
        .cursor(Cursor::ResizeColumn)
        .focusable(true)
        .focus_highlight(c(INK).alpha(0.20))
        .hover(c(INK).alpha(0.08))
        .on_drag(move |s: &mut State, d, _| match d.phase {
            rune::Phase::Down => d.data = width,
            rune::Phase::Move => apply(s, d.data + d.dx * direction),
            rune::Phase::Up if d.cancelled() => apply(s, d.data),
            rune::Phase::Up => {
                let requested = d.data + d.dx * direction;
                if sidebar && requested < 150.0 {
                    s.layout.collapsed = true;
                } else {
                    apply(s, requested);
                }
                remember(s);
            }
        })
        .on_key(move |s: &mut State, k, _| {
            let delta = match k.key {
                Key::Left => -16.0,
                Key::Right => 16.0,
                _ => return false,
            };
            apply(s, width + delta * direction);
            remember(s);
            true
        })
}

fn error(s: &State, inset: f32) -> Option<El> {
    s.error.as_ref().map(|e| {
        row()
            .w_full()
            .px(inset)
            .py(5.0)
            .gap(6.0)
            .center_y()
            .child(
                label(e, 12.0, 450.0, ERROR)
                    .grow(1.0)
                    .min_w(0.0)
                    .wrap_anywhere(),
            )
            .child(if s.connected() {
                tool("dismiss-error", "Dismiss error", &glyphs::CLOSE)
                    .on_press(|s: &mut State| s.error = None)
            } else {
                action("retry-open", "Retry", false).on_press(State::retry)
            })
    })
}

fn header(s: &State, narrow: bool, detail: Option<i64>) -> El {
    let mut bar = chrome(if detail.is_some() {
        "thread-header"
    } else {
        "feed-header"
    });
    if s.browse.jump_open
        && (detail.is_some() || !matches!(s.page, Page::Thread(_) | Page::Task(_)))
    {
        return bar
            .child(
                text_input(&s.browse.jump, font(13.0, 450.0))
                    .single_line()
                    .key("jump-id")
                    .label("Message ID")
                    .placeholder("Message ID", c(MUTED))
                    .grow(1.0)
                    .min_w(0.0)
                    .h(30.0)
                    .padding(6.0)
                    .fill(c(TINT))
                    .on_edit(|s: &mut State| &mut s.browse.jump)
                    .on_input_key(|s: &mut State, k, _| {
                        if k.key != Key::Enter {
                            return false;
                        }
                        s.jump_to_id();
                        true
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
    if let Some(id) = detail {
        if let Page::Task(_) = s.page {
            let meta = s
                .desk
                .selected_task
                .as_ref()
                .filter(|t| t.id == id)
                .map_or_else(|| format!("#{id}"), |t| format!("in #{}", t.channel));
            return bar
                .child(label("task", 13.0, 600.0, INK))
                .child(mono(meta, FAINT).grow(1.0).min_w(0.0).truncate())
                .child(
                    tool("back-to-feed", "Close task", &glyphs::CLOSE).on_press_tx(
                        |s: &mut State, tx| {
                            if let Page::Task(id) = s.page {
                                s.go(Page::Tasks);
                                tx.focus_when_shown(format!("task-row-{id}"), true);
                            }
                        },
                    ),
                );
        }
        let root = s.thread.first();
        let task = root.is_some_and(|m| m.task.is_some());
        bar = bar
            .child(label(
                if task { "task" } else { "thread" },
                13.0,
                600.0,
                INK,
            ))
            .child(
                mono(
                    root.map_or_else(|| format!("#{id}"), |m| format!("in #{}", m.channel)),
                    FAINT,
                )
                .grow(1.0)
                .min_w(0.0)
                .truncate(),
            );
        if !task && root.is_some_and(|m| !m.deleted) {
            bar = bar.child(
                tool("make-task", "Make this thread a task", &icons::LIST_TODO)
                    .on_press(move |s: &mut State| s.create_task(Some(id))),
            );
        }
        return bar.child(
            tool("back-to-feed", "Close thread", &glyphs::CLOSE).on_press_tx(State::close_thread),
        );
    }
    if narrow || s.layout.collapsed {
        bar = bar.child(
            tool("open-nav", "Show the sidebar", &icons::PANEL_LEFT_OPEN).on_press_tx(
                move |s: &mut State, tx| {
                    if narrow {
                        s.nav = true;
                    } else {
                        s.layout.collapsed = false;
                        remember(s);
                    }
                    tx.focus_when_shown("all-threads", true);
                },
            ),
        );
    }
    let browsing = s.page.is_feed() || matches!(s.page, Page::Thread(_));
    let title = if browsing {
        title_for(s)
    } else if matches!(s.page, Page::Tasks | Page::Task(_) | Page::NewTask) {
        "tasks".into()
    } else if s.page == Page::Activity {
        "activity history".into()
    } else if matches!(s.page, Page::Swarms | Page::Swarm(_)) {
        "swarms".into()
    } else {
        "settings".into()
    };
    bar = bar.child(
        label(title, 13.0, 600.0, INK)
            .max_w(if narrow { 160.0 } else { 230.0 })
            .min_w(0.0)
            .truncate(),
    );
    if browsing {
        let meta = match &s.channel {
            None => "the latest across every channel".into(),
            Some(ch) => s
                .channels
                .iter()
                .find(|c| c.name == *ch)
                .map_or_else(String::new, |ch| {
                    let plural = |n| if n == 1 { "" } else { "s" };
                    format!(
                        "{} thread{} · {} message{}",
                        ch.threads,
                        plural(ch.threads),
                        ch.messages,
                        plural(ch.messages)
                    )
                }),
        };
        bar = bar
            .child(mono(meta, FAINT).grow(1.0).min_w(0.0).truncate())
            .child(filters::button())
            .child(
                row()
                    .center_y()
                    .gap(5.0)
                    .help("Watching the local database for changes")
                    .child(el().size(6.0, 6.0).radius(3.0).fill(c(if s.connected() {
                        INK
                    } else {
                        ERROR
                    })))
                    .child(mono(if s.connected() { "live" } else { "offline" }, FAINT)),
            );
    } else if matches!(s.page, Page::Tasks | Page::Task(_)) {
        bar = bar
            .child(mono("oldest first", FAINT).grow(1.0).min_w(0.0).truncate())
            .child(
                tool("task-criteria", "Filter tasks", &icons::LIST_FILTER)
                    .on_press(State::edit_task_filters),
            )
            .child(
                row()
                    .center_y()
                    .gap(5.0)
                    .child(el().size(6.0, 6.0).radius(3.0).fill(c(if s.connected() {
                        INK
                    } else {
                        ERROR
                    })))
                    .child(mono(if s.connected() { "live" } else { "offline" }, FAINT)),
            );
    } else {
        bar = bar
            .child(el().grow(1.0))
            .child(
                tool("zoom-out", "Zoom out", &icons::MINUS)
                    .disabled(!s.can_zoom(false))
                    .on_press(|s: &mut State| s.zoom_step(-1)),
            )
            .child(
                click(row().key("zoom-reset").w(42.).h(28.).center(), "Reset zoom")
                    .help("Reset window zoom")
                    .child(mono(format!("{}%", s.layout.zoom_percent), DIM))
                    .on_press(|s: &mut State| s.zoom_step(0)),
            )
            .child(
                tool("zoom-in", "Zoom in", &icons::PLUS)
                    .disabled(!s.can_zoom(true))
                    .on_press(|s: &mut State| s.zoom_step(1)),
            )
            .child(tool("back-to-feed", "Back", &glyphs::CLOSE).on_press(State::back));
    }
    bar
}

fn main_pane(s: &State, narrow: bool) -> El {
    let inset = 20.0;
    let mut pane = col()
        .key("feed-pane")
        .grow(1.0)
        .min_w(0.0)
        .h_full()
        .clip_rounded(0.0)
        .child(header(s, narrow, None));
    if !matches!(s.page, Page::Thread(_) | Page::Task(_))
        && let Some(e) = error(s, inset)
    {
        pane = pane.child(e);
    }
    let body = if s.access_blocked {
        label("View unavailable", 13.0, 500.0, MUTED).padding(inset)
    } else if s.delivery.open && s.edits_root() {
        delivery::view(s, inset)
    } else if (s.people.picking && s.edits_root()) || s.page == Page::People {
        people::view(s, inset)
    } else if s.browse.filter_open {
        filters::view(s, inset)
    } else if s.page.is_feed() || matches!(s.page, Page::Thread(_)) {
        if s.browse.search_open || !s.query.text.is_empty() {
            pane = pane.child(
                row()
                    .w_full()
                    .center_y()
                    .gap(6.0)
                    .px(inset)
                    .py(6.0)
                    .child(search(s))
                    .child(
                        tool("close-search", "Close search", &glyphs::CLOSE)
                            .on_press(State::close_search),
                    ),
            );
        }
        if s.browse.has_filters() || s.tasks_only {
            pane = pane.child(
                row()
                    .w_full()
                    .center_y()
                    .gap(6.0)
                    .px(inset)
                    .child(filters::summary(s).grow(1.0).min_w(0.0))
                    .child(
                        tool("reset-query", "Clear filters", &glyphs::CLOSE)
                            .on_press(State::clear_filters),
                    ),
            );
        }
        feed(s, inset)
    } else if matches!(s.page, Page::Tasks | Page::Task(_)) {
        workspace::task_list_for(s, inset, narrow || matches!(s.page, Page::Task(_)))
    } else if matches!(s.page, Page::Swarms | Page::Swarm(_)) {
        swarms::view(s, inset, narrow)
    } else {
        workspace::view(s, inset)
    };
    pane.child(body)
}

fn thread_pane(s: &State, id: i64) -> El {
    let inset = 20.0;
    let mut pane = col()
        .key("thread-pane")
        .h_full()
        .min_w(0.0)
        .clip_rounded(0.0)
        .fill(c(BG))
        .child(header(s, false, Some(id)));
    if let Some(e) = error(s, inset) {
        pane = pane.child(e);
    }
    let body = if s.access_blocked {
        label("View unavailable", 13.0, 500.0, MUTED).padding(inset)
    } else if s.delivery.open && !s.edits_root() {
        delivery::view(s, inset)
    } else if s.people.picking && !s.edits_root() {
        people::view(s, inset)
    } else if let Some(p) = &s.preview {
        preview::view(p, inset)
    } else if let Some((p, m)) = s
        .properties
        .as_ref()
        .and_then(|p| s.thread.iter().find(|m| m.id == p.id).map(|m| (p, m)))
    {
        properties::view(s, p, m, inset)
    } else if matches!(s.page, Page::Task(_)) {
        if s.desk.task_options {
            workspace::task_detail(s, inset)
        } else {
            col()
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .child(workspace::task_banner(s, inset))
                .child(messages::task_conversation(s, id, inset))
        }
    } else {
        conversation(s, id, inset)
    };
    pane.child(body)
}

pub(super) fn window(s: &State, width: f32) -> El {
    let narrow = width <= BREAKPOINT;
    let with_sidebar = !narrow && !s.layout.collapsed;
    let split = matches!(s.page, Page::Thread(_) | Page::Task(_));
    let sidebar_width = if with_sidebar { s.layout.sidebar } else { 0.0 };
    let max_thread = (width * 0.7).min(width - sidebar_width - 200.0).max(320.0);
    let thread_width = s.layout.thread.min(max_thread);
    let mut layout = row().key("board-window").w_full().h_full().fill(c(BG));
    if with_sidebar {
        layout = layout.child(sidebar(s, false, sidebar_width));
    }
    if !split || !narrow {
        layout = layout.child(main_pane(s, narrow));
    }
    if let Page::Thread(id) | Page::Task(id) = s.page {
        let detail = thread_pane(s, id);
        layout = layout.child(if narrow {
            detail.w_full()
        } else {
            detail
                .w(thread_width)
                .border_sides([0.0, 0.0, 0.0, 1.0], c(EDGE))
        });
    }
    if with_sidebar {
        layout =
            layout.child(resize(true, sidebar_width, 400.0).absolute(sidebar_width - 4.0, 0.0));
    }
    if !narrow && split {
        layout = layout.child(
            resize(false, thread_width, max_thread).absolute(width - thread_width - 3.0, 0.0),
        );
    }
    if narrow && s.nav {
        layout = layout
            .child(
                el().key("nav-scrim")
                    .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
                    .fill(Color::BLACK.alpha(0.22))
                    .on_press(|s: &mut State| s.nav = false),
            )
            .child(
                sidebar(s, true, (width * 0.84).min(280.0))
                    .absolute(0.0, 0.0)
                    .modal_tx(true, |s: &mut State, tx| {
                        s.nav = false;
                        tx.focus_when_shown("open-nav", true);
                    }),
            );
    }
    layout
}
