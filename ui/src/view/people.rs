use super::*;
use workspace::field;

pub(super) fn view(s: &State, inset: f32) -> El {
    let d = &s.people;
    let mut list = col().w_full().px(inset).py(18.0).gap(12.0);
    let mut head = row().w_full().center_y().gap(6.0).child(
        label(
            if d.picking { "Mention" } else { "People" },
            22.0,
            630.0,
            INK,
        )
        .grow(1.0),
    );
    if d.picking {
        head = head.child(
            tool("close-people", "Back to composer", &glyphs::CLOSE).on_press_tx(
                |s: &mut State, tx| {
                    s.people = Default::default();
                    tx.focus_when_shown("message-body", true);
                },
            ),
        );
    } else if !s.actor.starts_with("source:") && !d.editing {
        head = head.child(
            action("edit-person", "My details", false).on_press_tx(State::edit_presentation),
        );
    }
    list = list.child(head);
    if d.editing {
        list = list
            .child(field(
                "person-name",
                "Display name",
                &d.name,
                |s| &mut s.people.name,
                false,
            ))
            .child(field(
                "person-handle",
                "Handle",
                &d.handle,
                |s| &mut s.people.handle,
                false,
            ))
            .child(
                row()
                    .w_full()
                    .gap(8.0)
                    .child(el().grow(1.0))
                    .child(
                        action("cancel-person", "Cancel", false)
                            .on_press(|s: &mut State| s.people.editing = false),
                    )
                    .child(action("save-person", "Save", true).on_press(State::save_presentation)),
            );
        return scroll("people-scroll", list);
    }
    list = list.child(
        text_input(&d.query, font(14.0, 450.0))
            .single_line()
            .key("person-query")
            .label("Find a person or agent")
            .placeholder("Find a person or agent", c(MUTED))
            .w_full()
            .h(36.0)
            .padding(8.0)
            .border(1.0, c(EDGE))
            .radius(2.0)
            .on_edit(|s: &mut State| &mut s.people.query)
            .on_input_change(|s: &mut State, _| {
                s.people.offset = 0;
                s.refresh();
            })
            .on_input_key(|s: &mut State, k, tx| {
                if k.key == Key::Enter && s.people.picking && s.people.rows.len() == 1 {
                    let id = s.people.rows[0].id.clone();
                    s.mention_person(&id, tx);
                    true
                } else {
                    false
                }
            }),
    );
    if d.rows.is_empty() {
        list = list.child(label("No matching identities", 15.0, 500.0, MUTED));
    }
    for (index, person) in d.rows.iter().enumerate().skip(d.offset).take(30) {
        let via = person
            .via
            .as_ref()
            .map(|s| format!(" via {s}"))
            .unwrap_or_default();
        let status = if person.verified {
            "   Verified locally"
        } else if !person.local {
            "   Imported"
        } else {
            ""
        };
        let row = row()
            .w_full()
            .key(format!("person-{index}"))
            .py(9.0)
            .center_y()
            .gap(9.0)
            .border_sides([0.0, 0.0, 1.0, 0.0], c(EDGE))
            .child(avatar(&person.name))
            .child(
                col()
                    .grow(1.0)
                    .min_w(0.0)
                    .gap(4.0)
                    .child(
                        label(&person.name, 15.0, 600.0, INK)
                            .w_full()
                            .wrap_anywhere(),
                    )
                    .child(
                        mono(
                            format!("@{}   {}{via}{status}", person.handle, person.kind),
                            MUTED,
                        )
                        .w_full()
                        .wrap_anywhere(),
                    )
                    .child(mono(&person.id, MUTED).w_full().wrap_anywhere()),
            );
        list = list.child(if d.picking {
            let id = person.id.clone();
            click(row, &format!("Mention {}", person.name))
                .on_press_tx(move |s: &mut State, tx| s.mention_person(&id, tx))
        } else {
            row
        });
    }
    if d.rows.len() > 30 {
        list = list.child(
            row()
                .w_full()
                .center_y()
                .gap(5.0)
                .child(
                    mono(
                        format!(
                            "{} to {} of {}",
                            d.offset + 1,
                            (d.offset + 30).min(d.rows.len()),
                            d.rows.len()
                        ),
                        MUTED,
                    )
                    .grow(1.0),
                )
                .child(
                    tool("previous-people", "Previous people", &glyphs::CHEVRON_UP)
                        .disabled(d.offset == 0)
                        .on_press_tx(|s: &mut State, tx| {
                            s.people.offset = s.people.offset.saturating_sub(30);
                            tx.scroll_to_y("people-scroll", 0.0);
                        }),
                )
                .child(
                    tool("next-people", "Next people", &glyphs::CHEVRON_DOWN)
                        .disabled(d.offset + 30 >= d.rows.len())
                        .on_press_tx(|s: &mut State, tx| {
                            s.people.offset += 30;
                            tx.scroll_to_y("people-scroll", 0.0);
                        }),
                ),
        );
    }
    scroll("people-scroll", list)
}
