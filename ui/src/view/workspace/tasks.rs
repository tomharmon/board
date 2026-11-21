use super::*;

fn command(id: i64, key: &str, name: &str, op: TaskCommand, primary: bool) -> El {
    action(key, name, primary).on_press(move |s: &mut State| s.task_command(id, op))
}
fn routing(s: &State, new: bool) -> El {
    let f = &s.desk.form;
    let mut out = col().w_full().gap(8.0).child(field(
        "assignee",
        "Assigned profile",
        &f.name,
        |s| &mut s.desk.form.name,
        false,
    ));
    if let Page::Task(id) = s.page {
        out = out.child(command(
            id,
            "save-assignment",
            "Set assignment",
            TaskCommand::Assign,
            false,
        ));
    }
    out = out.child(field(
        "task-needs",
        "Prerequisite IDs",
        &f.extra,
        |s| &mut s.desk.form.extra,
        false,
    ));
    if !new && let Page::Task(id) = s.page {
        out = out.child(command(
            id,
            "save-needs",
            "Set prerequisites",
            TaskCommand::Needs,
            false,
        ));
    }
    out
}

pub(super) fn view(s: &State) -> El {
    let d = &s.desk;
    if s.page == Page::NewTask {
        return col()
            .w_full()
            .gap(12.0)
            .child(heading("New task", None))
            .child(field(
                "task-channel",
                "Channel",
                &d.form.channel,
                |s| &mut s.desk.form.channel,
                false,
            ))
            .child(field(
                "task-body",
                "Task",
                &d.form.body,
                |s| &mut s.desk.form.body,
                true,
            ))
            .child(routing(s, true))
            .child(
                action("create-task", "Create task", true)
                    .disabled(d.form.body.text.trim().is_empty())
                    .on_press(|s: &mut State| s.create_task(None)),
            );
    }
    if s.page == Page::Tasks {
        if d.editing {
            let f = &d.form;
            return col()
                .w_full()
                .gap(12.0)
                .child(heading(
                    "Task filters",
                    Some(
                        tool("close-task-filters", "Close filters", &glyphs::CLOSE)
                            .on_press(|s: &mut State| s.desk.editing = false),
                    ),
                ))
                .child(field(
                    "criteria-channel",
                    "Channel or scope",
                    &f.channel,
                    |s| &mut s.desk.form.channel,
                    false,
                ))
                .child(field(
                    "criteria-owner",
                    "Owner",
                    &f.name,
                    |s| &mut s.desk.form.name,
                    false,
                ))
                .child(field(
                    "criteria-assignee",
                    "Assignee",
                    &f.value,
                    |s| &mut s.desk.form.value,
                    false,
                ))
                .child(field(
                    "criteria-needs",
                    "Needs task ID",
                    &f.extra,
                    |s| &mut s.desk.form.extra,
                    false,
                ))
                .child(field(
                    "criteria-labels",
                    "Labels (one per line)",
                    &f.body,
                    |s| &mut s.desk.form.body,
                    true,
                ))
                .child(
                    row()
                        .w_full()
                        .gap(6.0)
                        .child(
                            action("clear-task-filters", "Clear filters", false).on_press(
                                |s: &mut State| {
                                    s.desk.criteria = Default::default();
                                    s.desk.editing = false;
                                    s.desk.first_page();
                                    s.refresh();
                                },
                            ),
                        )
                        .child(el().grow(1.0))
                        .child(
                            action("apply-task-filters", "Apply", true)
                                .on_press(State::save_task_filters),
                        ),
                );
        }
        let mut panel = col()
            .key("task-board")
            .w_full()
            .gap(12.0)
            .child(heading(
                "Tasks",
                Some(
                    action("new-task", "New task", true).on_press_tx(|s: &mut State, tx| {
                        s.go(Page::NewTask);
                        tx.focus_when_shown("task-body", true);
                    }),
                ),
            ))
            .child(
                row()
                    .w_full()
                    .center_y()
                    .gap(6.0)
                    .child(seg(
                        &["Open", "Ready", "Claimed", "All"],
                        d.task_filter,
                        "Task state",
                        "task-filter",
                        |s, i| {
                            s.desk.task_filter = i;
                            s.desk.first_page();
                            s.refresh();
                        },
                    ))
                    .child(el().grow(1.0))
                    .child(
                        action("task-criteria", "Filters", false)
                            .on_press(State::edit_task_filters),
                    ),
            );
        let c = &d.criteria;
        let mut terms = Vec::new();
        if let Some(ch) = &c.channel {
            terms.push(format!("#{ch}"));
        }
        if let Some(o) = &c.owner {
            terms.push(format!("Owner {o}"));
        }
        if let Some(a) = &c.assignee {
            terms.push(format!("Assignee {a}"));
        }
        if let Some(n) = c.needs {
            terms.push(format!("Needs #{n}"));
        }
        for t in &c.label_terms {
            terms.push(t.values.as_ref().map_or_else(
                || format!("Has {}", t.key),
                |v| format!("{}={}", t.key, v.join("|")),
            ));
        }
        if !terms.is_empty() {
            panel = panel.child(mono(terms.join("   "), MUTED).w_full().wrap_anywhere());
        }
        if d.tasks.is_empty() {
            panel = panel.child(empty("No matching tasks"));
        }
        for task in &d.tasks {
            let id = task.id;
            let who = task
                .holder
                .as_deref()
                .or(task.assignee.as_deref())
                .unwrap_or("Unassigned");
            panel = panel.child(listrow(
                &format!("task-row-{id}"),
                &task.text,
                &format!("#{id}    {}    {}    #{}", task.state, who, task.channel),
                move |s| s.go(Page::Task(id)),
            ));
        }
        return panel;
    }
    let Some(t) = d.tasks.first() else {
        return empty("Task unavailable");
    };
    let id = t.id;
    let owner = board_local::owned_by(&t.owner, &s.actor);
    let held = d.has_claim(&s.actor, t);
    let idle = matches!(t.state.as_str(), "ready" | "waiting" | "blocked");
    let mut panel = col()
        .w_full()
        .gap(11.0)
        .child(heading(
            &format!("Task #{id}"),
            Some(
                action("task-thread", "Open thread", false)
                    .on_press(move |s: &mut State| s.go(Page::Thread(id))),
            ),
        ))
        .child(label(&t.text, 17.0, 560.0, INK).w_full().wrap_anywhere())
        .child(task_details(t));
    if t.state == "ready" && t.assignee.as_ref().is_none_or(|a| a == &s.actor) {
        panel = panel.child(command(
            id,
            "claim-task",
            "Claim for 5 minutes",
            TaskCommand::Claim,
            true,
        ));
    }
    if held {
        panel = panel.child(
            row()
                .w_full()
                .gap(7.0)
                .child(command(
                    id,
                    "renew-task",
                    "Renew 5 min",
                    TaskCommand::Renew,
                    false,
                ))
                .child(command(
                    id,
                    "release-task",
                    "Release",
                    TaskCommand::Release,
                    false,
                )),
        );
    }
    if held
        || owner
            && matches!(
                t.state.as_str(),
                "ready" | "waiting" | "blocked" | "claimed" | "failed"
            )
    {
        panel = panel.child(field(
            "task-result",
            "Result or reason",
            &d.form.body,
            |s| &mut s.desk.form.body,
            true,
        ));
        if held {
            panel = panel.child(
                row()
                    .w_full()
                    .gap(6.0)
                    .child(command(
                        id,
                        "complete-task",
                        "Complete",
                        TaskCommand::Complete,
                        true,
                    ))
                    .child(
                        command(id, "fail-task", "Fail", TaskCommand::Fail, false)
                            .disabled(d.form.body.text.trim().is_empty()),
                    ),
            );
        }
        let mut controls = row().w_full().gap(6.0);
        if held || owner && matches!(t.state.as_str(), "ready" | "waiting") {
            controls = controls.child(
                command(id, "block-task", "Block", TaskCommand::Block, false)
                    .disabled(d.form.body.text.trim().is_empty()),
            );
        }
        if owner && matches!(t.state.as_str(), "failed" | "blocked") {
            controls = controls.child(command(
                id,
                "resume-task",
                "Resume",
                TaskCommand::Resume,
                false,
            ));
        }
        if owner {
            controls = controls.child(if d.confirm.as_deref() == Some("cancel-task") {
                command(
                    id,
                    "confirm-cancel-task",
                    "Confirm cancel",
                    TaskCommand::Cancel,
                    true,
                )
            } else {
                action("cancel-task", "Cancel task...", false)
                    .disabled(d.form.body.text.trim().is_empty())
                    .on_press(|s: &mut State| s.desk.confirm = Some("cancel-task".into()))
            });
        }
        panel = panel.child(controls);
    }
    if owner && (idle || t.state == "claimed") {
        panel = panel.child(rule()).child(
            action(
                "edit-routing",
                if d.editing {
                    "Hide routing"
                } else {
                    "Edit routing"
                },
                false,
            )
            .on_press(|s: &mut State| s.desk.editing = !s.desk.editing),
        );
        if d.editing {
            panel = panel.child(routing(s, false));
        }
    }
    panel
}
