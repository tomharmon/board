use super::*;
use rune_kit::Checkbox;

pub(super) fn options(s: &State) -> El {
    let active = s.delivery.no_sync || !s.delivery.labels.text.is_empty();
    tool("delivery-options", "Delivery options", &icons::TAG)
        .fill(if active { c(TINT) } else { Color::TRANSPARENT })
        .on_press(State::open_delivery)
}
fn labels(s: &State) -> El {
    let name = "Labels (one key=value per line)";
    let field = text_input(&s.delivery.labels, font(14.0, 450.0))
        .key("compose-labels")
        .label(name)
        .w_full()
        .h(74.0)
        .padding(8.0)
        .radius(4.0)
        .border(1.0, c(EDGE))
        .on_edit(|s: &mut State| &mut s.delivery.labels)
        .on_input_change(|s: &mut State, _| s.draft_changed());
    col()
        .w_full()
        .gap(4.0)
        .child(label(name, 13.0, 570.0, INK).w_full().wrap_anywhere())
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
                .child(label("Delivery options", 15.0, 600.0, INK).grow(1.0))
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
                .build(&Kit::neutral(dark()))
                .key("compose-no-sync"),
        )
        .child(labels(s));
    if let Some(choices) = suggestions::labels(s, suggestions::Field::Delivery) {
        form = form.child(choices);
    }
    if let Some(id) = d.after {
        form = form.child(guard(id));
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
