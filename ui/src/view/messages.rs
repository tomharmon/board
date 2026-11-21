use super::*;
mod composebox;
use composebox::{activation, composer, edit};

pub(super) fn message_text(m: &Message) -> &str {
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
pub(super) fn author(m: &Message) -> &str {
    m.author.as_ref().map_or(&m.actor, |a| &a.name)
}
pub(super) fn stamp(ts: &str) -> String {
    ts.get(..16).unwrap_or(ts).replace('T', " ")
}
#[derive(Clone, Copy, PartialEq)]
enum Place {
    Activity,
    Channel,
    Thread,
}

fn parsed(ts: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.to_utc())
}
pub(super) fn relative(ts: &str, now: chrono::DateTime<chrono::Utc>) -> String {
    let Some(t) = parsed(ts) else {
        return stamp(ts);
    };
    let seconds = (now - t).num_seconds().max(0);
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86400),
    }
}
fn same_day(a: &str, b: &str) -> bool {
    a.get(..10) == b.get(..10)
}
fn continues(before: Option<&Message>, m: &Message, previous_had_replies: bool) -> bool {
    before.is_some_and(|p| {
        !previous_had_replies
            && !p.deleted
            && !m.deleted
            && p.actor == m.actor
            && p.channel == m.channel
            && p.re == m.re
            && same_day(&p.created_at, &m.created_at)
            && parsed(&p.created_at)
                .zip(parsed(&m.created_at))
                .is_some_and(|(a, b)| (0..300).contains(&(b - a).num_seconds()))
    })
}
fn divider(ts: &str, now: chrono::DateTime<chrono::Utc>, inset: f32) -> El {
    let title = parsed(ts).map_or_else(
        || ts.to_owned(),
        |t| match (now.date_naive() - t.date_naive()).num_days() {
            0 => "today".into(),
            1 => "yesterday".into(),
            _ => t.format("%b %-d, %Y").to_string(),
        },
    );
    row()
        .w_full()
        .px(inset)
        .pt(14.0)
        .pb(4.0)
        .gap(8.0)
        .center_y()
        .child(label(title, 11.0, 400.0, FAINT).font(font(11., 400.).lh(16.5)))
        .child(el().grow(1.0).h(1.0).fill(c(EDGE)))
}
fn transcript(s: &State, thread: bool, body: El, sticky: bool) -> El {
    let (key, edge, jump) = if thread {
        ("conversation", &s.browse.thread_edge, "thread-new-messages")
    } else {
        ("feed-scroll", &s.browse.feed_edge, "feed-new-messages")
    };
    let watch = move |s: &mut State, visible, tx: &mut Tx| {
        let edge = if thread {
            &mut s.browse.thread_edge
        } else {
            &mut s.browse.feed_edge
        };
        let was_behind = edge.show_jump();
        edge.visible(visible);
        if was_behind == edge.show_jump() {
            tx.live_only();
        }
    };
    let list = col()
        .key(key)
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .scroll(ScrollSpec {
            stick_bottom: sticky,
        })
        .child(el().grow(1.0))
        .child(body)
        .child(
            el().key(format!("{key}-edge"))
                .w_full()
                .h(1.)
                .on_visible(watch),
        );
    // The pager above this wrapper disappears after the last prepend. Keep the
    // viewport's full keyed path stable so Rune can restore the visible row.
    let panel = col()
        .key(if thread {
            "thread-transcript"
        } else {
            "feed-transcript"
        })
        .w_full()
        .grow(1.)
        .min_h(0.)
        .child(list);
    if !edge.show_jump() {
        return panel;
    }
    panel.child(
        row()
            .w_full()
            .center()
            .child(
                action(jump, "New messages ↓", false).on_press_tx(move |s: &mut State, tx| {
                    let edge = if thread {
                        &mut s.browse.thread_edge
                    } else {
                        &mut s.browse.feed_edge
                    };
                    edge.visible(true);
                    tx.scroll_to_bottom(key);
                }),
            ),
    )
}
pub(super) fn feed_key(m: &Message) -> String {
    if m.re.is_some() {
        format!("activity-reply-{}", m.id)
    } else {
        format!("thread-{}", m.id)
    }
}
fn next_feed(s: &State, key: &Key, id: i64, tx: &mut Tx) -> bool {
    let rows: Vec<_> = if s.is_timeline() {
        s.activity_messages.iter().collect()
    } else {
        s.threads.iter().map(|t| &t.message).collect()
    };
    let Some(i) = rows.iter().position(|m| m.id == id) else {
        return false;
    };
    let next = match key {
        Key::Down => i + 1,
        Key::Up if i > 0 => i - 1,
        Key::Up => {
            tx.focus("search", true);
            return true;
        }
        _ => return false,
    };
    if let Some(m) = rows.get(next) {
        let key = feed_key(m);
        tx.focus(&key, true);
        tx.scroll_into_view(key);
    }
    true
}

pub(super) fn feed(s: &State, inset: f32) -> El {
    let now = chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now());
    let is_timeline = s.is_timeline();
    // The bottom visibility marker occupies the final point of transcript padding.
    let mut list = col().w_full().pt(6.0).pb(11.0);
    let mut prev: Option<&Message> = None;
    let mut prev_activity: Option<&str> = None;
    let mut prev_replies = false;
    let rows: Vec<_> = if is_timeline {
        s.activity_messages.iter().map(|m| (m, None)).collect()
    } else {
        s.threads.iter().map(|t| (&t.message, Some(t))).collect()
    };
    if !s.connected() || rows.is_empty() {
        let msg = if !s.connected() {
            "Board unavailable"
        } else if !s.starts.is_empty() {
            "No threads on this page"
        } else if s.tasks_only {
            "No matching tasks"
        } else if s.query.text.is_empty() && !s.browse.has_filters() {
            "No threads yet"
        } else if is_timeline {
            "No matching messages"
        } else {
            "No matching threads"
        };
        list = list.child(label(msg, 13.0, 400.0, FAINT).px(inset).py(10.0));
    } else {
        let mut first = true;
        for (m, summary) in rows {
            let active = summary.and_then(|t| t.last.as_ref()).unwrap_or(m);
            if prev_activity.is_none_or(|p| !same_day(p, &active.created_at)) {
                list = list.child(divider(&active.created_at, now, inset));
            }
            let place = if is_timeline || s.channel.is_none() {
                Place::Activity
            } else {
                Place::Channel
            };
            let mut item = message(s, m, place, continues(prev, m, prev_replies), summary, now);
            if first
                && s.feed_size < 200
                && let Some(cursor) = s.older
            {
                let key = feed_key(m);
                let size = s.feed_size;
                item = item.on_visible(move |s: &mut State, visible, tx| {
                    if visible && s.feed_size == size && s.older == Some(cursor) {
                        tx.keep_on_screen(&key);
                        s.feed_size += 50;
                        s.refresh();
                    } else {
                        tx.live_only();
                    }
                });
            }
            first = false;
            list = list.child(item);
            prev = Some(m);
            prev_activity = Some(&active.created_at);
            prev_replies = summary.is_some_and(|t| t.replies > 0);
        }
    }
    let mut panel = col().w_full().grow(1.0).min_h(0.0);
    if s.older.is_some() || !s.starts.is_empty() {
        panel = panel.child(
            row()
                .w_full()
                .px(inset)
                .h(28.0)
                .center_y()
                .gap(4.0)
                .child(label(format!("Page {}", s.starts.len() + 1), 11.0, 400.0, FAINT).grow(1.0))
                .child(
                    tool("older-threads", "Older messages", &glyphs::CHEVRON_UP)
                        .disabled(s.older.is_none())
                        .on_press_tx(|s: &mut State, tx| {
                            if let Some(before) = s.older {
                                s.starts.push(before);
                                s.refresh();
                                tx.scroll_to_bottom("feed-scroll");
                            }
                        }),
                )
                .child(
                    tool("newer-threads", "Newer messages", &glyphs::CHEVRON_DOWN)
                        .disabled(s.starts.is_empty())
                        .on_press_tx(|s: &mut State, tx| {
                            s.starts.pop();
                            s.refresh();
                            tx.scroll_to_bottom("feed-scroll");
                        }),
                ),
        );
    }
    panel = panel.child(transcript(s, false, list, true));
    if s.connected() {
        panel = panel.child(if s.edits_root() {
            composer(s, inset)
        } else {
            activation(s, inset, true)
        });
    }
    panel
}

pub(super) fn task_details(t: &Task) -> El {
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

fn at_message(s: &mut State, id: i64, from_feed: bool) -> bool {
    if from_feed {
        s.open_message_thread(id);
        s.browse.reveal = None;
    }
    s.page.thread_id().is_some() && s.thread.iter().any(|m| m.id == id)
}
fn controls(s: &State, m: &Message, from_feed: bool) -> El {
    let p = if from_feed { "feed-" } else { "" };
    let id = m.id;
    let starred = m
        .reactions
        .get("star")
        .is_some_and(|who| who.contains(&s.actor));
    row()
        .pin(None, Some(0.), Some(10.), None)
        .center_y()
        .fill(c(BG))
        .border(1., c(EDGE))
        .group_opacity(0., 1.)
        .child(
            tool(
                &format!("{p}star-{id}"),
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
            .size(25., 22.)
            .disabled(m.deleted)
            .on_press(move |s: &mut State| s.star(id, starred)),
        )
        .child(
            tool(
                &format!("{p}reply-{id}"),
                &format!("Reply to #{id}"),
                if from_feed {
                    &icons::MESSAGE_SQUARE
                } else {
                    &icons::REPLY
                },
            )
            .size(25., 22.)
            .disabled(m.deleted)
            .on_press_tx(move |s: &mut State, tx| {
                if at_message(s, id, from_feed) && s.activate_draft(false) {
                    s.reply_to = Some(id);
                    s.draft_changed();
                    tx.focus_when_shown("message-body", true);
                }
            }),
        )
        .child(
            tool(
                &format!("{p}inspect-{id}"),
                &format!("Message #{id} details"),
                &icons::ELLIPSIS,
            )
            .size(25., 22.)
            .on_press_tx(move |s: &mut State, tx| {
                if at_message(s, id, from_feed) {
                    s.open_properties(id);
                    tx.focus_when_shown("close-properties", true);
                }
            }),
        )
}
fn chip(text: impl Into<String>) -> El {
    row()
        .center_y()
        .px(6.)
        .py(1.)
        .border(1., c(EDGE))
        .fill(c(PANEL))
        .child(
            label(text, 11., 400., MUTED)
                .font(font(11., 400.).lh(16.))
                .max_w(198.)
                .wrap_anywhere(),
        )
}
fn short_reaction(name: &str) -> &str {
    match name {
        "eyes" => "👀",
        "star" => "★",
        "thumbs_up" | "thumbsup" | "+1" => "👍",
        "heart" => "♥",
        "check" | "white_check_mark" => "✓",
        "party" | "tada" => "🎉",
        "laugh" => "😄",
        s => s,
    }
}
fn message(
    s: &State,
    m: &Message,
    place: Place,
    continued: bool,
    summary: Option<&ThreadSummary>,
    now: chrono::DateTime<chrono::Utc>,
) -> El {
    let id = m.id;
    let from_feed = place != Place::Thread;
    let p = if from_feed { "feed-" } else { "" };
    let mut content = col().grow(1.).min_w(0.);
    if !continued {
        let mut meta = row()
            .w_full()
            .min_h(19.5)
            .flex_wrap()
            .gap(8.)
            .center_y()
            .child(
                label(author(m), 13., 600., INK)
                    .max_w(220.)
                    .truncate()
                    .help(&m.actor),
            );
        if place == Place::Activity {
            meta = meta.child(
                row()
                    .center_y()
                    .gap(4.)
                    .child(label(
                        if m.re.is_some() { "replied in" } else { "in" },
                        12.,
                        400.,
                        FAINT,
                    ))
                    .child(
                        label(format!("#{}", m.channel), 12., 400., INK)
                            .max_w(190.)
                            .truncate(),
                    ),
            );
        }
        meta = meta.child(
            label(relative(&m.created_at, now), 11., 400., FAINT).help(format!(
                "{} UTC · #{}",
                stamp(&m.created_at),
                id
            )),
        );
        if let Some(parent) = m.reply_to
            && place == Place::Thread
        {
            meta = meta.child(label(format!("reply to #{parent}"), 11., 400., FAINT));
        }
        content = content.child(meta);
    }
    if m.no_sync || m.schedule.is_some() || m.from.is_some() {
        let mut badges = row().w_full().flex_wrap().center_y().gap(4.);
        if m.no_sync {
            badges = badges.child(chip("Local only"));
        }
        if let Some(send) = &m.schedule {
            badges = badges.child(chip(format!("Schedule #{}   Send {}", send.id, send.n)));
        }
        if let Some(origin) = &m.from {
            badges = badges.child(chip(format!("Source: {origin}")));
        }
        content = content.child(badges);
    }
    content = content.child(rich::scoped_body(s, m, p));
    for (i, l) in m.links.iter().enumerate().take(12) {
        let title = l.message.map_or_else(
            || "Quoted message".into(),
            |n| format!("Quoted message #{n}"),
        );
        let mut quote = col()
            .key(format!("{p}quote-{id}-{i}"))
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
    if m.labels.keys().any(|k| k != "board.task") || !m.reactions.is_empty() {
        let mut chips = row().w_full().flex_wrap().center_y().gap(4.).pt(4.);
        for (i, (k, v)) in m
            .labels
            .iter()
            .filter(|(k, _)| k.as_str() != "board.task")
            .enumerate()
        {
            let key = k.clone();
            chips = chips.child(
                click(
                    chip(format!("{k}={v}")).key(format!("{p}label-chip-{id}-{i}")),
                    &format!("Edit {k}"),
                )
                .on_press_tx(move |s: &mut State, tx| {
                    if at_message(s, id, from_feed) {
                        s.open_properties(id);
                        if let Some(value) = s
                            .thread
                            .iter()
                            .find(|m| m.id == id)
                            .and_then(|m| m.labels.get(&key))
                            && let Some(editor) = &mut s.properties
                        {
                            editor.select_label(&key, value);
                        }
                        tx.focus_when_shown("label-value", true);
                    }
                }),
            );
        }
        for (i, (name, actors)) in m.reactions.iter().enumerate() {
            let name = name.clone();
            let mine = actors.contains(&s.actor);
            let help = format!("{}: {}", name, actors.join(", "));
            let mut base = chip(format!("{} {}", short_reaction(&name), actors.len()));
            if mine {
                base = base.fill(c(TINT)).border(1., c(MUTED));
            }
            chips = chips.child(
                click(base.key(format!("{p}reaction-{id}-{i}")).help(&help), &help).on_press(
                    move |s: &mut State| {
                        s.toggle_named_reaction(id, &name, mine);
                    },
                ),
            );
        }
        content = content.child(chips);
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
                            &format!("{p}preview-{id}-{file}"),
                            "Preview attachment",
                            &glyphs::EYE,
                        )
                        .on_press_tx(move |s: &mut State, tx| {
                            if at_message(s, id, from_feed) {
                                s.open_attachment(id, file, tx);
                            }
                        }),
                    )
                    .child(
                        tool(
                            &format!("{p}save-file-{file}"),
                            "Save attachment",
                            &glyphs::FOLDER_OPEN,
                        )
                        .on_press_tx(move |s: &mut State, tx| s.save_attachment(file, &name, tx)),
                    ),
            );
        }
    }
    if let Some(t) = &m.task
        && from_feed
    {
        content = content.child(label(format!("Task: {}", t.state), 12., 500., MUTED).pt(3.));
    }
    if let Some(t) = summary.filter(|t| t.replies > 0) {
        let mut replies = row().center_y().gap(6.).pt(4.);
        if let Some(last) = &t.last {
            replies = replies.child(avatar(author(last)).size(16., 16.));
        }
        replies = replies.child(label(
            format!(
                "{} {}",
                t.replies,
                if t.replies == 1 { "reply" } else { "replies" }
            ),
            12.,
            600.,
            INK,
        ));
        if let Some(last) = &t.last {
            replies = replies.child(label(
                format!("· last {}", relative(&last.created_at, now)),
                11.,
                400.,
                FAINT,
            ));
        }
        content = content.child(replies);
    }
    for (i, n) in m.file_notices.iter().enumerate() {
        content = content.child(
            mono(format!("File not copied: {}   {}", n.name, n.reason), MUTED)
                .key(format!("{p}file-notice-{id}-{i}"))
                .w_full()
                .wrap_anywhere(),
        );
    }
    let key = if from_feed {
        feed_key(m)
    } else {
        format!("message-{id}")
    };
    let gutter = col().w(40.).center_y().child(if continued {
        label(m.created_at.get(11..16).unwrap_or(""), 10., 400., FAINT).group_opacity(0., 1.)
    } else {
        avatar(author(m))
    });
    let base = row()
        .key(key)
        .w_full()
        .pl(8.)
        .pr(20.)
        .pt(if continued { 1. } else { 6. })
        .pb(if continued { 1. } else { 4. })
        .gap(8.)
        .hover_group();
    let mut entry = if from_feed {
        let root = m.re.is_none();
        click(base, &format!("Open message #{id}"))
            .on_press_tx(move |s: &mut State, tx| {
                s.open_message_thread(id);
                if root && s.page == Page::Thread(id) && s.activate_draft(false) {
                    s.browse.focused = None;
                    s.browse.reveal = None;
                    tx.focus_when_shown("message-body", true);
                }
            })
            .on_key(move |s: &mut State, k, tx| next_feed(s, &k.key, id, tx))
    } else {
        base
    };
    if !from_feed && s.browse.focused == Some(id) {
        entry = entry.fill(c(PANEL));
    }
    entry
        .child(gutter)
        .child(content)
        .child(controls(s, m, from_feed))
}
pub(super) fn conversation(s: &State, id: i64, inset: f32) -> El {
    thread_body(s, id, inset, true)
}

pub(super) fn task_conversation(s: &State, id: i64, inset: f32) -> El {
    thread_body(s, id, inset, false)
}

fn thread_body(s: &State, id: i64, inset: f32, show_task_facts: bool) -> El {
    let now = chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now());
    let mut messages = col().w_full().pb(11.);
    let mut previous: Option<&Message> = None;
    for (index, m) in s.thread.iter().enumerate() {
        if previous.is_none_or(|p| !same_day(&p.created_at, &m.created_at)) {
            messages = messages.child(divider(&m.created_at, now, inset));
        }
        messages = messages.child(message(
            s,
            m,
            Place::Thread,
            index > 1 && continues(previous, m, false),
            None,
            now,
        ));
        if index == 0 {
            let count = s.thread.len() - 1;
            let title = if s.browse.older || s.browse.newer {
                "replies".to_owned()
            } else {
                format!("{count} {}", if count == 1 { "reply" } else { "replies" })
            };
            messages = messages.child(
                row()
                    .w_full()
                    .px(inset)
                    .pt(14.)
                    .pb(4.)
                    .center_y()
                    .gap(8.)
                    .child(label(title, 11., 400., FAINT).font(font(11., 400.).lh(16.5)))
                    .child(el().grow(1.).h(1.).fill(c(EDGE))),
            );
        }
        previous = Some(m);
    }
    let mut content = col().w_full().grow(1.).min_h(0.);
    if !s.browse.targets.is_empty() {
        let mut targets = col()
            .key("thread-destinations")
            .w_full()
            .px(inset)
            .py(4.)
            .max_h(88.)
            .scroll(ScrollSpec {
                stick_bottom: false,
            });
        for (i, t) in s.browse.targets.iter().enumerate() {
            let mut line = row()
                .w_full()
                .center_y()
                .gap(8.)
                .child(label("sync", 11., 400., FAINT).w(42.))
                .child(
                    label(&t.target, 12., 500., INK)
                        .grow(1.)
                        .min_w(0.)
                        .truncate()
                        .help(&t.source),
                )
                .child(
                    label(
                        &t.state,
                        11.,
                        400.,
                        if matches!(t.state.as_str(), "failed" | "uncertain") {
                            ERROR
                        } else {
                            MUTED
                        },
                    )
                    .help("Recorded locally; no provider lookup"),
                );
            if let Some(url) = t.url.clone() {
                line = line.child(
                    tool(
                        &format!("copy-destination-{i}"),
                        "Copy recorded destination link",
                        &glyphs::COPY,
                    )
                    .on_press_tx(move |s: &mut State, tx| s.copy_destination(id, &url, tx)),
                );
            }
            targets = targets.child(line);
        }
        content = content.child(targets);
    }
    if show_task_facts && let Some(task) = s.thread.first().and_then(|m| m.task.as_ref()) {
        let tid = task.id;
        content = content.child(
            col()
                .w_full()
                .px(inset)
                .pb(5.)
                .child(
                    col()
                        .key("thread-task-summary")
                        .w_full()
                        .max_h(128.0)
                        .scroll(ScrollSpec {
                            stick_bottom: false,
                        })
                        .child(task_details(task)),
                )
                .child(
                    action("open-task", "Manage task", false)
                        .on_press(move |s: &mut State| s.go(Page::Task(tid))),
                ),
        );
    }
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
    content = content.child(transcript(
        s,
        true,
        messages,
        s.browse.position == Position::Latest,
    ));
    if !s.typing.is_empty() {
        content = content.child(
            label(
                s.typing
                    .iter()
                    .map(|t| format!("{}: {}", t.actor, t.status))
                    .collect::<Vec<_>>()
                    .join("   "),
                11.,
                400.,
                MUTED,
            )
            .key("typing")
            .w_full()
            .px(inset)
            .truncate(),
        );
    }
    content.child(if s.edits_root() {
        activation(s, inset, false)
    } else {
        edit(s, true, inset)
    })
}
