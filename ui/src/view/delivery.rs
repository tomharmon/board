use super::*;
use rune::TextEdit;

pub(super) fn options(s: &State) -> El {
    let active = s.delivery.later || s.delivery.no_sync || !s.delivery.labels.text.is_empty();
    tool(
        "delivery-options",
        "Delivery options",
        if s.delivery.no_sync {
            &glyphs::LOCK
        } else {
            &glyphs::FILE_TEXT
        },
    )
    .fill(if active { c(TINT) } else { Color::TRANSPARENT })
    .on_press(State::open_delivery)
}
fn input(
    key: &str,
    label_: &str,
    value: &TextEdit,
    multi: bool,
    bind: fn(&mut State) -> &mut TextEdit,
) -> El {
    let mut field = text_input(value, TextStyle::new(14.0, 450.0))
        .key(key)
        .label(label_)
        .w_full()
        .padding(8.0)
        .radius(4.0)
        .border(1.0, c(EDGE))
        .on_edit(bind)
        .on_input_change(|s: &mut State, _| s.draft_changed());
    field = if multi {
        field.h(74.0)
    } else {
        field.single_line().h(35.0)
    };
    col()
        .w_full()
        .gap(4.0)
        .child(label(label_, 13.0, 570.0, INK).w_full().wrap_anywhere())
        .child(field)
}
pub(super) fn view(s: &State, inset: f32) -> El {
    let d = &s.delivery;
    let mut form = col()
        .w_full()
        .px(inset)
        .py(18.0)
        .gap(14.0)
        .child(
            row()
                .w_full()
                .center_y()
                .gap(6.0)
                .child(label("Delivery options", 22.0, 630.0, INK).grow(1.0))
                .child(
                    tool("close-delivery", "Back to composer", &glyphs::CLOSE).on_press_tx(
                        |s: &mut State, tx| {
                            s.delivery.open = false;
                            tx.focus_when_shown("message-body", true);
                        },
                    ),
                ),
        )
        .child(
            Checkbox::new(d.no_sync)
                .label("Local only (never sync)")
                .on_toggle(|s: &mut State| {
                    s.delivery.no_sync = !s.delivery.no_sync;
                    s.draft_changed();
                })
                .build(&Kit::neutral(false))
                .key("compose-no-sync"),
        )
        .child(input(
            "compose-labels",
            "Labels (one key=value per line)",
            &d.labels,
            true,
            |s| &mut s.delivery.labels,
        ))
        .child(
            Checkbox::new(d.later)
                .label("Schedule delivery")
                .on_toggle(|s: &mut State| {
                    s.delivery.later = !s.delivery.later;
                    s.draft_changed();
                })
                .build(&Kit::neutral(false))
                .disabled(d.after.is_some())
                .key("compose-later"),
        );
    if let Some(id) = d.after {
        form = form.child(guard(id));
    }
    if d.later {
        form = form
            .child(input(
                "compose-due",
                "First send (RFC 3339)",
                &d.due,
                false,
                |s| &mut s.delivery.due,
            ))
            .child(
                row()
                    .w_full()
                    .gap(8.0)
                    .child(col().grow(1.0).min_w(0.0).child(input(
                        "compose-every",
                        "Repeat interval (seconds)",
                        &d.every,
                        false,
                        |s| &mut s.delivery.every,
                    )))
                    .child(col().grow(1.0).min_w(0.0).child(input(
                        "compose-times",
                        "Maximum sends",
                        &d.times,
                        false,
                        |s| &mut s.delivery.times,
                    ))),
            )
            .child(input(
                "compose-until",
                "Or repeat until (RFC 3339)",
                &d.until,
                false,
                |s| &mut s.delivery.until,
            ));
    }
    if s.error.is_some() {
        form = form.child(
            action(
                "reload-composer",
                "Discard edits and reload saved draft",
                false,
            )
            .on_press(State::reload_composer),
        );
    }
    form = form.child(
        row()
            .w_full()
            .center_y()
            .gap(8.0)
            .child(el().grow(1.0))
            .child(
                action("done-delivery", "Done", true).on_press_tx(|s: &mut State, tx| {
                    match s.flush_draft() {
                        Ok(()) => {
                            s.delivery.open = false;
                            s.error = None;
                            tx.focus_when_shown("message-body", true);
                        }
                        Err(e) => s.error = Some(e.to_string()),
                    }
                }),
            ),
    );
    scroll("delivery-scroll", form)
}

pub(super) fn guard(id: i64) -> El {
    row()
        .w_full()
        .center_y()
        .gap(5.0)
        .child(
            mono(format!("Only if #{id} is still newest"), ACCENT)
                .key("pinned-guard")
                .grow(1.0)
                .min_w(0.0)
                .wrap_anywhere(),
        )
        .child(
            tool(
                "remove-guard",
                "Remove newest-only condition",
                &glyphs::CLOSE,
            )
            .on_press(State::remove_guard),
        )
}
