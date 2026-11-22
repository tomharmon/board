use super::*;
use board_local::swarm::AttentionItem;

pub(super) fn banner(s: &State, run_id: Option<i64>) -> El {
    let items: Vec<_> = s
        .desk
        .attention
        .iter()
        .filter(|i| i.action_required && run_id.is_none_or(|id| i.run_id == Some(id)))
        .collect();
    if items.is_empty() {
        return el().h(0.);
    }
    let mut out = col()
        .key("attention-banner")
        .w_full()
        .px(20.)
        .py(10.)
        .gap(5.)
        .border_sides([0., 0., 1., 0.], c(EDGE));
    out = out.child(
        click(
            row().key("open-attention").w_full().center_y().gap(8.),
            "Open needs attention",
        )
        .child(icon(&icons::LIST_TODO, 16.).color(c(ERROR)))
        .child(label(format!("Needs attention · {}", items.len()), 13., 550., INK).grow(1.))
        .child(icon(&glyphs::ARROW_RIGHT, 14.).color(c(MUTED)))
        .on_press(|s: &mut State| s.go(Page::Attention)),
    );
    for i in items.iter().take(2) {
        out = out.child(
            label(format!("{}: {}", i.waiting, i.summary), 12., 400., DIM)
                .w_full()
                .wrap_anywhere()
                .max_lines(2),
        );
    }
    out
}

fn item(s: &State, i: &AttentionItem) -> El {
    let id = i.message_id;
    let mut head = row()
        .w_full()
        .center_y()
        .gap(8.)
        .child(label(
            &i.kind,
            11.,
            550.,
            if i.action_required { ERROR } else { MUTED },
        ))
        .child(el().grow(1.))
        .child(
            tool(
                &format!("attention-source-{id}"),
                "Open source thread",
                &glyphs::ARROW_RIGHT,
            )
            .on_press(move |s: &mut State| s.open_message_thread(id)),
        );
    if let Some(request) = i.decision_id.filter(|_| i.action_required) {
        head = head.child(
            action(&format!("attention-respond-{request}"), "Respond", false).on_press_tx(
                move |s: &mut State, tx| {
                    s.begin_decision_answer(request);
                    tx.focus_when_shown("decision-answer", true);
                },
            ),
        );
    }
    let mut out = col()
        .key(format!("attention-item-{id}-{}", i.kind))
        .w_full()
        .py(12.)
        .gap(7.)
        .child(head)
        .child(label(&i.summary, 14., 500., INK).w_full().wrap_anywhere())
        .child(
            mono(
                format!("Waiting: {} · To: {}", i.waiting, i.assigned_to),
                MUTED,
            )
            .w_full()
            .wrap_anywhere(),
        )
        .child(
            mono(
                format!(
                    "{} · respond at #{}",
                    i.run.as_deref().unwrap_or("task"),
                    id
                ),
                FAINT,
            )
            .w_full()
            .wrap_anywhere(),
        );
    if i.decision_id.is_some() && s.desk.selected_decision == i.decision_id {
        out = out
            .child(
                text_input(&s.desk.decision_answer, font(13., 400.))
                    .key("decision-answer")
                    .label("Decision response")
                    .w_full()
                    .h(76.)
                    .padding(8.)
                    .border(1., c(CONTROL))
                    .color(c(INK))
                    .on_edit(|s: &mut State| &mut s.desk.decision_answer),
            )
            .child(
                row()
                    .w_full()
                    .gap(8.)
                    .child(
                        action("cancel-decision-answer", "Cancel", false)
                            .on_press(State::cancel_decision_answer),
                    )
                    .child(el().grow(1.))
                    .child(
                        submit(
                            "send-decision-answer",
                            "Send decision",
                            !s.desk.decision_answer.text.trim().is_empty(),
                        )
                        .on_press(State::answer_decision),
                    ),
            );
    }
    out.child(rule())
}

pub(super) fn view(s: &State, inset: f32) -> El {
    let mut list = col().w_full().px(inset).py(16.).gap(4.);
    let actionable = s
        .desk
        .attention
        .iter()
        .filter(|i| i.action_required)
        .count();
    list = list.child(label(format!("Your actions · {actionable}"), 15., 550., INK).py(8.));
    if actionable == 0 {
        list = list.child(
            mono("No decisions or blockers assigned to you", MUTED)
                .py(16.)
                .w_full()
                .wrap_anywhere(),
        );
    }
    for i in s.desk.attention.iter().filter(|i| i.action_required) {
        list = list.child(item(s, i));
    }
    let following = s
        .desk
        .attention
        .iter()
        .filter(|i| !i.action_required)
        .count();
    if following > 0 {
        list = list.child(
            label(format!("Agent follow-up · {following}"), 14., 550., DIM)
                .pt(24.)
                .pb(8.),
        );
        for i in s.desk.attention.iter().filter(|i| !i.action_required) {
            list = list.child(item(s, i));
        }
    }
    scroll("attention-scroll", list)
}

pub(super) fn receipt(s: &State) -> El {
    let Some(receipt) = &s.desk.swarm.receipt else {
        return el();
    };
    let mut list = col()
        .w_full()
        .padding(12.)
        .gap(10.)
        .child(label("Run receipt", 15., 550., INK))
        .child(mono(format!("Run: {}", receipt.run_status), MUTED));
    if receipt.handoffs.is_empty() {
        list = list.child(
            mono("No revision-bound verification recorded", MUTED)
                .w_full()
                .wrap_anywhere(),
        );
    }
    for h in receipt.handoffs.iter().rev() {
        let mut entry = col()
            .key(format!("receipt-handoff-{}", h.id))
            .w_full()
            .py(10.)
            .gap(7.)
            .child(
                label(
                    &h.handoff.summary,
                    14.,
                    550.,
                    if h.superseded { MUTED } else { INK },
                )
                .w_full()
                .wrap_anywhere(),
            )
            .child(
                mono(
                    format!(
                        "{} · {}{}",
                        h.actor,
                        h.status,
                        if h.superseded { " · superseded" } else { "" }
                    ),
                    if h.status == "fail" { ERROR } else { DIM },
                )
                .w_full()
                .wrap_anywhere(),
            )
            .child(
                mono(
                    format!("{}: {}", h.handoff.revision.kind, h.handoff.revision.value),
                    MUTED,
                )
                .w_full()
                .wrap_anywhere()
                .selectable(true),
            );
        for change in &h.handoff.changes {
            entry = entry.child(label(change, 12., 400., DIM).w_full().wrap_anywhere());
        }
        if let Some(v) = &h.verification {
            entry = entry
                .child(
                    mono(
                        format!(
                            "Verified by {}",
                            h.verified_by.as_deref().unwrap_or("unknown")
                        ),
                        MUTED,
                    )
                    .w_full()
                    .wrap_anywhere(),
                )
                .child(label(&v.summary, 13., 400., DIM).w_full().wrap_anywhere());
            for (n, check) in v.checks.iter().enumerate() {
                entry = entry.child(
                    col()
                        .key(format!("receipt-check-{}-{n}", h.id))
                        .w_full()
                        .gap(4.)
                        .py(5.)
                        .child(
                            label(
                                format!("{} · {}", check.status, check.name),
                                12.,
                                500.,
                                if check.status == "fail" { ERROR } else { DIM },
                            )
                            .w_full()
                            .wrap_anywhere(),
                        )
                        .child(
                            mono(&check.command, MUTED)
                                .w_full()
                                .wrap_anywhere()
                                .selectable(true),
                        )
                        .child(
                            mono(check.output.as_deref().unwrap_or(""), FAINT)
                                .w_full()
                                .wrap_anywhere()
                                .selectable(true),
                        ),
                );
            }
        }
        let source = h.verification_id.unwrap_or(h.id);
        entry = entry.child(
            tool(
                &format!("receipt-source-{}", h.id),
                "Open evidence thread",
                &glyphs::ARROW_RIGHT,
            )
            .on_press(move |s: &mut State| s.open_message_thread(source)),
        );
        list = list.child(entry).child(rule());
    }
    list = list.child(
        label(
            format!(
                "Still open · {} decisions, {} tasks",
                receipt.decisions.len(),
                receipt.open_tasks.len()
            ),
            13.,
            550.,
            INK,
        )
        .w_full()
        .wrap_anywhere(),
    );
    for d in &receipt.decisions {
        list = list.child(
            mono(format!("{}: {}", d.request.to, d.request.summary), DIM)
                .w_full()
                .wrap_anywhere(),
        );
    }
    if !receipt.decisions.is_empty() {
        list = list.child(
            action("receipt-decisions", "Review decisions", false)
                .on_press(|s: &mut State| s.go(Page::Attention)),
        );
    }
    for &id in &receipt.open_tasks {
        list = list.child(
            action(&format!("receipt-task-{id}"), &format!("Task #{id}"), false)
                .on_press(move |s: &mut State| s.go(Page::Task(id))),
        );
    }
    scroll("swarm-receipt-scroll", list)
}
