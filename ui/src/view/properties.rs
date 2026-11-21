use super::*;
use crate::state::{Properties, Property};
use rune::TextEdit;
use rune_kit::SegmentedControl;

fn field(
    input: &TextEdit,
    id: &'static str,
    name: &str,
    bind: fn(&mut Properties) -> &mut TextEdit,
) -> El {
    col()
        .w_full()
        .gap(5.0)
        .child(label(name, 13.0, 600.0, INK))
        .child(
            text_input(input, TextStyle::new(14.0, 450.0))
                .single_line()
                .key(id)
                .label(name)
                .w_full()
                .h(35.0)
                .px(9.0)
                .radius(5.0)
                .border(1.0, c(EDGE))
                .fill(c("#fafbf9"))
                .on_edit(move |s: &mut State| bind(s.properties.as_mut().expect("active editor"))),
        )
}

fn labels(p: &Properties, m: &Message) -> El {
    let mut list = col().w_full().gap(5.0);
    if m.labels.is_empty() {
        list = list.child(label("No labels", 13.0, 450.0, MUTED));
    }
    for (i, (key, value)) in m.labels.iter().enumerate() {
        let (k, v) = (key.clone(), value.clone());
        list = list.child(
            click(
                row()
                    .key(format!("label-row-{i}"))
                    .w_full()
                    .center_y()
                    .gap(10.0)
                    .py(7.0)
                    .border_sides([0.0, 0.0, 1.0, 0.0], c(EDGE)),
                &format!("Edit label {key}"),
            )
            .child(mono(key, "#376345").w(100.0).wrap_anywhere())
            .child(
                label(value, 14.0, 500.0, INK)
                    .grow(1.0)
                    .min_w(0.0)
                    .wrap_anywhere(),
            )
            .disabled(key.starts_with("board."))
            .on_press_tx(move |s: &mut State, tx| {
                if let Some(p) = &mut s.properties {
                    p.select_label(&k, &v);
                }
                tx.focus_when_shown("label-value", true);
            }),
        );
    }
    let empty = p.label_key.text.trim().is_empty();
    col()
        .w_full()
        .gap(13.0)
        .child(list)
        .child(field(&p.label_key, "label-key", "Label key", |p| {
            &mut p.label_key
        }))
        .child(field(&p.label_value, "label-value", "Value", |p| {
            &mut p.label_value
        }))
        .child(
            row()
                .w_full()
                .gap(8.0)
                .child(
                    action("set-label", "Set label", true)
                        .disabled(empty)
                        .on_press(|s: &mut State| s.change_label(false)),
                )
                .child(
                    action("unset-label", "Remove label", false)
                        .disabled(empty)
                        .on_press(|s: &mut State| s.change_label(true)),
                ),
        )
}

fn reactions(p: &Properties, m: &Message, actor: &str) -> El {
    let mut list = col().w_full().gap(5.0);
    if m.reactions.is_empty() {
        list = list.child(label("No reactions", 13.0, 450.0, MUTED));
    }
    for (i, (name, actors)) in m.reactions.iter().enumerate() {
        let key = name.clone();
        let mine = actors.iter().any(|a| a == actor);
        list =
            list.child(
                click(
                    row()
                        .key(format!("reaction-row-{i}"))
                        .w_full()
                        .py(6.0)
                        .gap(12.0)
                        .center_y(),
                    &format!("Select reaction {name}"),
                )
                .child(
                    icon(if mine { &glyphs::CHECK } else { &glyphs::DASH }, 14.0)
                        .color(c(if mine { ACCENT } else { MUTED })),
                )
                .child(mono(name, INK).max_w(105.0).wrap_anywhere())
                .child(
                    label(actors.join(", "), 13.0, 450.0, MUTED)
                        .grow(1.0)
                        .min_w(0.0)
                        .wrap_anywhere(),
                )
                .on_press(move |s: &mut State| {
                    if let Some(p) = &mut s.properties {
                        p.reaction = TextEdit::new(&key);
                    }
                }),
            );
    }
    let empty = p.reaction.text.trim().is_empty();
    col()
        .w_full()
        .gap(13.0)
        .child(list)
        .child(field(&p.reaction, "reaction-name", "Reaction", |p| {
            &mut p.reaction
        }))
        .child(
            row()
                .w_full()
                .gap(8.0)
                .child(
                    action("add-reaction", "Add reaction", true)
                        .disabled(empty)
                        .on_press(|s: &mut State| s.change_reaction(false)),
                )
                .child(
                    action("remove-reaction", "Remove reaction", false)
                        .disabled(empty)
                        .on_press(|s: &mut State| s.change_reaction(true)),
                ),
        )
}

fn editor(p: &Properties) -> El {
    if p.confirm_delete {
        return col()
            .w_full()
            .gap(15.0)
            .child(label(
                format!("Delete message #{}?", p.id),
                18.0,
                600.0,
                INK,
            ))
            .child(
                label(
                    "The message and its attachments will be removed. Replies stay in the thread.",
                    14.0,
                    450.0,
                    MUTED,
                )
                .w_full()
                .wrap_anywhere(),
            )
            .child(
                row()
                    .gap(8.0)
                    .child(action("keep-message", "Keep message", false).on_press(
                        |s: &mut State| {
                            if let Some(p) = &mut s.properties {
                                p.confirm_delete = false;
                            }
                        },
                    ))
                    .child(
                        action("confirm-delete", "Delete message", true)
                            .on_press_tx(|s: &mut State, tx| s.change_body(true, tx)),
                    ),
            );
    }
    col()
        .w_full()
        .gap(13.0)
        .child(
            text_input(&p.edit, TextStyle::new(15.0, 450.0).lh(21.0))
                .key("edit-body")
                .w_full()
                .h(160.0)
                .padding(10.0)
                .radius(5.0)
                .border(1.0, c(EDGE))
                .fill(c("#fafbf9"))
                .label("Message source")
                .on_edit(|s: &mut State| &mut s.properties.as_mut().expect("active editor").edit)
                .on_input_key(|s: &mut State, k, tx| {
                    if k.key == Key::Enter && (k.mods.ctrl || k.mods.cmd) {
                        s.change_body(false, tx);
                        true
                    } else {
                        false
                    }
                }),
        )
        .child(
            row()
                .w_full()
                .gap(8.0)
                .child(
                    action("save-edit", "Save edit", true)
                        .on_press_tx(|s: &mut State, tx| s.change_body(false, tx)),
                )
                .child(action("delete-message", "Delete...", false).on_press_tx(
                    |s: &mut State, tx| {
                        if let Some(p) = &mut s.properties {
                            p.confirm_delete = true;
                        }
                        tx.focus_when_shown("keep-message", true);
                    },
                )),
        )
}

pub(super) fn view(s: &State, p: &Properties, m: &Message, inset: f32) -> El {
    let id = m.id;
    let mut panel = col()
        .w_full()
        .px(inset)
        .py(20.0)
        .gap(13.0)
        .child(
            row()
                .w_full()
                .center_y()
                .gap(8.0)
                .child(
                    label(format!("Message #{}", m.id), 22.0, 630.0, INK)
                        .grow(1.0)
                        .min_w(0.0)
                        .truncate(),
                )
                .child(
                    tool("close-properties", "Back to thread", &glyphs::CLOSE)
                        .on_press_tx(State::close_properties),
                ),
        )
        .child(
            row()
                .w_full()
                .center_y()
                .gap(6.0)
                .child(
                    mono(format!("@{}    Revision {}", m.actor, m.revision), MUTED)
                        .grow(1.0)
                        .min_w(0.0)
                        .wrap_anywhere(),
                )
                .child(
                    tool("copy-message-id", "Copy message ID", &glyphs::COPY)
                        .on_press_tx(move |_: &mut State, tx| tx.copy(id.to_string())),
                )
                .child(
                    tool("copy-message-body", "Copy message text", &glyphs::FILE_TEXT)
                        .disabled(m.deleted)
                        .on_press_tx(move |s: &mut State, tx| s.copy_message_text(id, tx)),
                ),
        );
    for j in &p.deliveries {
        panel = panel.child(
            mono(
                format!(
                    "{}   {}   {}   Attempts: {}",
                    j.source, j.event, j.state, j.attempts
                ),
                MUTED,
            )
            .key(format!("delivery-notice-{}", j.id))
            .w_full()
            .wrap_anywhere(),
        );
    }
    if m.deleted {
        return scroll(
            "details-scroll",
            panel.child(label("Message deleted", 15.0, 500.0, MUTED)),
        );
    }
    panel = panel.child(
        action("guard-reply", "Reply only if still newest", false)
            .help(format!(
                "Reply only if #{} remains the newest message in this thread",
                id
            ))
            .on_press_tx(move |s: &mut State, tx| s.guard_reply(id, tx)),
    );
    let can_edit = board_local::owned_by(&m.actor, &s.actor);
    let tabs: &[&str] = if can_edit {
        &["Labels", "Reactions", "Edit"]
    } else {
        &["Labels", "Reactions"]
    };
    let (index, body) = match p.property {
        Property::Labels => (0, labels(p, m)),
        Property::Reactions => (1, reactions(p, m, &s.actor)),
        Property::Body => (2, editor(p)),
    };
    panel = panel
        .child(
            SegmentedControl::new(tabs, Some(index))
                .label("Message details")
                .fit()
                .on_pick(|s: &mut State, i, tx| {
                    if let Some(p) = &mut s.properties {
                        let (prop, key) = match i {
                            0 => (Property::Labels, "label-key"),
                            1 => (Property::Reactions, "reaction-name"),
                            _ => (Property::Body, "edit-body"),
                        };
                        p.property = prop;
                        p.confirm_delete = false;
                        s.error = None;
                        tx.focus_when_shown(key, true);
                    }
                })
                .build(&Kit::neutral(false)),
        )
        .child(rule());
    if s.error.is_some() || p.revision != m.revision {
        let id = m.id;
        let prop = p.property;
        panel = panel.child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(
                    mono(format!("Opened at revision {}", p.revision), MUTED)
                        .grow(1.0)
                        .min_w(0.0)
                        .wrap_anywhere(),
                )
                .child(
                    action("reload-message", "Reload current", false)
                        .help("Discard edits and load the current message")
                        .on_press(move |s: &mut State| {
                            s.refresh();
                            s.open_properties(id);
                            if let Some(p) = &mut s.properties {
                                p.property = prop;
                            }
                        }),
                ),
        );
    }
    scroll("details-scroll", panel.child(body))
}
