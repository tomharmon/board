use super::*;

fn can_send(s: &State) -> bool {
    !s.body.text.trim().is_empty() || !s.files.is_empty()
}
fn body(s: &State, reply: bool) -> El {
    let placeholder = if reply {
        "reply to thread…".into()
    } else if s.channel.is_some() {
        format!("message #{}…", s.root_channel())
    } else {
        "start a thread…".into()
    };
    let height = ((s.body.text.lines().count().clamp(2, 6) * 21) as f32 + 16.).clamp(58., 160.);
    text_input(&s.body, font(13.6, 400.).lh(21.))
        .key("message-body")
        .label(if reply { "Reply" } else { "Message" })
        .placeholder(placeholder, c(FAINT))
        .color(c(INK))
        .on_edit(|s: &mut State| &mut s.body)
        .on_input_paste(|s: &mut State, content, _| s.paste_file(content))
        .on_input_change(move |s: &mut State, _| {
            s.draft_changed();
            s.update_inline(false);
            if reply {
                s.note_typing();
            }
        })
        .w_full()
        .h(height)
        .px(12.)
        .pt(10.)
        .pb(2.)
        .no_focus_ring()
        .on_input_key(move |s: &mut State, k, tx| {
            if s.inline_key(k, tx) {
                true
            } else if crate::state::markdown_key(&mut s.body, k) {
                s.draft_changed();
                s.update_inline(false);
                if reply {
                    s.note_typing();
                }
                true
            } else if k.key == Key::Up
                && s.body.text.is_empty()
                && s.body.preedit().is_none()
                && !k.mods.shift
                && !k.mods.cmd
                && !k.mods.ctrl
                && !k.mods.alt
            {
                s.edit_last_visible(tx);
                true
            } else if k.key == Key::Enter && !k.mods.shift && s.body.preedit().is_none() {
                if can_send(s) {
                    s.send(tx);
                }
                true
            } else if k.key == Key::Escape && s.body.preedit().is_none() {
                if matches!(s.page, Page::Task(_)) {
                    match s.flush_draft() {
                        Ok(()) => tx.focus_when_shown("task-options", true),
                        Err(e) => s.error = Some(format!("Draft not saved: {e:#}")),
                    }
                } else {
                    s.back();
                }
                true
            } else {
                false
            }
        })
}
fn send(s: &State) -> El {
    submit(
        "send",
        if s.delivery.later { "Schedule" } else { "send" },
        can_send(s),
    )
    .on_press_tx(State::send)
}
fn attach(s: &State) -> El {
    tool("attach-file", "Attach file", &icons::PAPERCLIP)
        .disabled(s.files.len() >= board_local::MAX_ATTACHMENTS)
        .on_press_tx(State::pick_file)
}
fn mention() -> El {
    tool(
        "mention-person",
        "Mention a person or agent",
        &icons::AT_SIGN,
    )
    .on_press_tx(State::pick_person)
}
fn files(s: &State) -> El {
    let mut list = col().w_full().gap(2.);
    for (i, (name, bytes)) in s.files.iter().enumerate() {
        list = list.child(
            row()
                .w_full()
                .center_y()
                .gap(6.)
                .px(7.)
                .fill(c(PANEL))
                .border(1., c(EDGE))
                .child(icon(&glyphs::FILE, 13.).color(c(MUTED)))
                .child(
                    label(format!("{name} ({} bytes)", bytes.len()), 12., 400., INK)
                        .grow(1.)
                        .min_w(0.)
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
    col()
        .key("queued-files")
        .w_full()
        .px(12.)
        .max_h(72.)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .child(list)
}
fn channel(s: &State) -> El {
    let field = short_input(&s.destination, "destination", "Channel", "channel")
        .on_edit(|s: &mut State| &mut s.destination)
        .on_input_change(|s: &mut State, _| s.draft_changed())
        .on_input_key(|s: &mut State, k, tx| {
            if k.key == Key::Enter && s.destination.preedit().is_none() {
                tx.focus_when_shown("message-body", true);
                true
            } else {
                false
            }
        });
    col()
        .w_full()
        .px(8.)
        .pt(8.)
        .child(short_field("destination", &s.destination, Some("#"), field).max_w(320.))
}
fn composer_frame(focus: bool) -> El {
    col()
        .w_full()
        .fill(c(if focus { PANEL } else { BG }))
        .border(1., c(if focus { MUTED } else { CONTROL }))
        .border_dash(if focus { 6. } else { 3. }, if focus { 3. } else { 2. })
}
pub(super) fn edit(s: &State, reply: bool, inset: f32) -> El {
    let mut area = composer_frame(s.body.focused || s.destination.focused)
        .key(if reply {
            "reply-composer"
        } else {
            "main-composer"
        })
        .on_drop_files(State::drop_files);
    if !reply
        && (s.channel.is_none()
            || s.page == Page::Compose
            || s.channel.as_deref() != Some(s.destination.text.as_str()))
    {
        area = area.child(channel(s));
    }
    if reply && let Some(target) = s.reply_to {
        area = area.child(
            row()
                .w_full()
                .px(8.)
                .center_y()
                .gap(3.)
                .child(label(format!("Replying to #{target}"), 11., 400., MUTED))
                .child(
                    tool("clear-reply-to", "Reply to thread instead", &glyphs::CLOSE).on_press(
                        |s: &mut State| {
                            s.reply_to = None;
                            s.draft_changed();
                        },
                    ),
                ),
        );
    }
    if let Some(id) = s.delivery.after {
        area = area.child(delivery::guard(id));
    }
    let mut labels = s
        .delivery
        .labels
        .text
        .lines()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .peekable();
    if s.delivery.no_sync || labels.peek().is_some() {
        let mut flags = row().w_full().flex_wrap().px(8.).pt(6.).gap(4.);
        if s.delivery.no_sync {
            flags = flags.child(chip("Local only").key("draft-local-only"));
        }
        for (i, v) in labels.by_ref().take(3).enumerate() {
            let short = v.chars().take(60).collect::<String>();
            flags = flags.child(
                click(
                    chip(short).key(format!("draft-label-{i}")),
                    "Edit draft labels",
                )
                .on_press(State::open_delivery),
            );
        }
        if labels.peek().is_some() {
            flags = flags.child(
                click(
                    chip(format!("+{}", labels.count())).key("draft-more-labels"),
                    "Edit draft labels",
                )
                .on_press(State::open_delivery),
            );
        }
        area = area.child(flags);
    }
    area = area.child(body(s, reply));
    if s.inline_open() {
        let mut choices = row().w_full().px(8.).flex_wrap().gap(3.);
        for (i, person) in s.people.inline.rows.iter().enumerate() {
            let id = person.id.clone();
            choices = choices.child(
                click(
                    row().key(format!("mention-choice-{i}")).px(5.).py(2.).fill(
                        if s.people.inline.selected == i {
                            c(TINT)
                        } else {
                            Color::TRANSPARENT
                        },
                    ),
                    &format!("Mention {}", person.name),
                )
                .child(
                    label(format!("@{}", person.handle), 12., 400., INK)
                        .max_w(190.)
                        .truncate(),
                )
                .on_press_tx(move |s: &mut State, tx| s.complete_mention(&id, tx)),
            );
        }
        area = area.child(choices);
    }
    if !s.files.is_empty() {
        area = area.child(files(s));
    }
    let mut footer = row()
        .w_full()
        .px(8.)
        .pt(2.)
        .pb(8.)
        .gap(2.)
        .center_y()
        .child(attach(s))
        .child(delivery::options(s))
        .child(
            tool(
                "composer-private",
                if s.delivery.no_sync {
                    "Allow configured delivery"
                } else {
                    "Keep this message local"
                },
                &icons::CLOUD_OFF,
            )
            .fill(if s.delivery.no_sync {
                c(TINT)
            } else {
                Color::TRANSPARENT
            })
            .on_press(|s: &mut State| {
                s.delivery.no_sync = !s.delivery.no_sync;
                s.draft_changed();
            }),
        );
    if !reply {
        footer = footer.child(
            tool("compose-task", "New task", &icons::LIST_TODO)
                .on_press(|s: &mut State| s.go(Page::NewTask)),
        );
    }
    footer = footer
        .child(
            tool(
                "compose-schedule",
                "Schedule and delivery settings",
                &icons::CLOCK,
            )
            .on_press(State::open_delivery),
        )
        .child(mention())
        .child(el().grow(1.))
        .child(send(s));
    col().w_full().px(inset).pb(inset).child(area.child(footer))
}
pub(super) fn composer(s: &State, inset: f32) -> El {
    edit(s, false, inset)
}
pub(super) fn activation(s: &State, inset: f32, root: bool) -> El {
    let begin = move |s: &mut State, tx: &mut Tx| {
        let ok = s.activate_draft(root);
        if ok {
            tx.focus_when_shown("message-body", true);
        }
        ok
    };
    let mut area = composer_frame(false).key(if root {
        "inactive-main-composer"
    } else {
        "inactive-thread-composer"
    });
    if root && s.channel.is_none() {
        area = area.child(
            col().w_full().px(8.).pt(8.).child(
                click(
                    row()
                        .key("feed-channel")
                        .w_full()
                        .max_w(320.)
                        .h(30.)
                        .px(8.)
                        .center_y()
                        .radius(4.)
                        .border(1., c(CONTROL))
                        .fill(c(PANEL)),
                    "Choose a channel",
                )
                .child(label("#channel", 13., 400., FAINT))
                .on_press_tx(move |s: &mut State, tx| {
                    if begin(s, tx) {
                        tx.focus_when_shown("destination", true);
                    }
                }),
            ),
        );
    }
    let placeholder = if !s.inactive_draft.is_empty() {
        s.inactive_draft.clone()
    } else if !root {
        "reply to thread…".into()
    } else {
        s.channel.as_ref().map_or_else(
            || "start a thread…".to_owned(),
            |ch| format!("message #{ch}…"),
        )
    };
    let prefix = if root { "feed" } else { "thread" };
    area = area.child(
        click(
            col()
                .key(format!("{prefix}-compose"))
                .w_full()
                .h(58.)
                .px(12.)
                .pt(10.),
            if root {
                "Start a thread"
            } else {
                "Reply to thread"
            },
        )
        .child(
            label(
                placeholder,
                14.,
                400.,
                if s.inactive_draft.is_empty() {
                    FAINT
                } else {
                    DIM
                },
            )
            .w_full()
            .wrap_anywhere()
            .max_lines(2),
        )
        .on_press_tx(move |s: &mut State, tx| {
            begin(s, tx);
        }),
    );
    let mut tools = row()
        .w_full()
        .px(8.)
        .pt(2.)
        .pb(8.)
        .gap(2.)
        .center_y()
        .child(
            tool(
                &format!("{prefix}-attach"),
                "Attach file",
                &icons::PAPERCLIP,
            )
            .on_press_tx(move |s: &mut State, tx| {
                if begin(s, tx) {
                    s.pick_file(tx);
                }
            }),
        )
        .child(
            tool(
                &format!("{prefix}-options"),
                "Labels and delivery options",
                &icons::TAG,
            )
            .on_press_tx(move |s: &mut State, tx| {
                if begin(s, tx) {
                    s.open_delivery();
                }
            }),
        )
        .child(
            tool(
                &format!("{prefix}-private"),
                "Compose locally only",
                &icons::CLOUD_OFF,
            )
            .on_press_tx(move |s: &mut State, tx| {
                if begin(s, tx) {
                    s.delivery.no_sync = true;
                    s.draft_changed();
                }
            }),
        );
    if root {
        tools = tools.child(
            tool("feed-task", "New task", &icons::LIST_TODO)
                .on_press(|s: &mut State| s.go(Page::NewTask)),
        );
    }
    area = area.child(
        tools
            .child(
                tool(
                    &format!("{prefix}-schedule"),
                    "Schedule and delivery settings",
                    &icons::CLOCK,
                )
                .on_press_tx(move |s: &mut State, tx| {
                    if begin(s, tx) {
                        s.open_delivery();
                    }
                }),
            )
            .child(el().grow(1.))
            .child(submit(&format!("{prefix}-send"), "send", false)),
    );
    col().w_full().px(inset).pb(inset).child(area)
}
