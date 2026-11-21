use super::*;

fn command(id: i64, key: &str, name: &str, op: TaskCommand, primary: bool) -> El {
    action(key, name, primary).on_press(move |s: &mut State| s.task_command(id, op))
}
fn routing(s: &State, new: bool) -> El {
    let f = if new {
        &s.desk.task_entry.form
    } else {
        &s.desk.form
    };
    let mut out = col().w_full().gap(8.0).child(field(
        "assignee",
        "Assigned profile",
        &f.name,
        if new {
            |s| &mut s.desk.task_entry.form.name
        } else {
            |s| &mut s.desk.form.name
        },
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
        if new {
            |s| &mut s.desk.task_entry.form.extra
        } else {
            |s| &mut s.desk.form.extra
        },
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

fn ready(s: &State) -> bool {
    let t = &s.desk.task_entry;
    !t.form.channel.text.trim().is_empty()
        && (!t.form.body.text.trim().is_empty() || !t.files.is_empty())
}
fn attachments(s: &State) -> El {
    let mut files = row().w_full().gap(5.0).flex_wrap();
    for (i, (name, _)) in s.desk.task_entry.files.iter().enumerate() {
        files = files.child(
            click(
                row()
                    .key(format!("task-file-{i}"))
                    .h(22.0)
                    .px(5.0)
                    .center_y()
                    .gap(5.0)
                    .fill(c(TINT)),
                &format!("Remove attachment {name}"),
            )
            .child(label(name, 11.0, 400.0, DIM).max_w(170.0).truncate())
            .child(icon(&glyphs::CLOSE, 11.0).color(c(MUTED)))
            .on_press(move |s: &mut State| {
                if i < s.desk.task_entry.files.len() {
                    s.desk.task_entry.files.remove(i);
                }
            }),
        );
    }
    files
}
pub(super) fn composer(s: &State, inset: f32) -> El {
    let t = &s.desk.task_entry;
    let mut body = col()
        .key("task-composer")
        .w_full()
        .padding(8.0)
        .gap(5.0)
        .border(1.0, c(EDGE))
        .border_dash(2.0, 2.0)
        .child(
            text_input(&t.form.channel, font(13.0, 400.0))
                .key("quick-task-channel")
                .label("Task channel")
                .placeholder("#channel", c(FAINT))
                .on_edit(|s: &mut State| &mut s.desk.task_entry.form.channel)
                .single_line()
                .color(c(INK))
                .fill(c(TINT))
                .padding(6.0)
                .h(28.0)
                .w_full()
                .max_w(320.0),
        )
        .child(
            text_input(&t.form.body, font(14.0, 400.0))
                .key("quick-task-body")
                .label("Task text")
                .placeholder("start a task…", c(FAINT))
                .on_edit(|s: &mut State| &mut s.desk.task_entry.form.body)
                .w_full()
                .h(50.0)
                .padding(4.0)
                .color(c(INK))
                .on_input_key(|s: &mut State, k, _| {
                    if k.key == Key::Enter
                        && !k.mods.shift
                        && s.desk.task_entry.form.body.preedit().is_none()
                    {
                        if ready(s) {
                            s.create_task(None)
                        }
                        true
                    } else {
                        false
                    }
                }),
        );
    if t.show_labels {
        body = body.child(field(
            "quick-task-labels",
            "Labels (one per line)",
            &t.labels,
            |s| &mut s.desk.task_entry.labels,
            true,
        ));
    }
    if !t.files.is_empty() {
        body = body.child(attachments(s));
    }
    let footer = row()
        .w_full()
        .gap(3.0)
        .center_y()
        .child(
            tool("quick-task-attach", "Attach task file", &icons::PAPERCLIP)
                .on_press_tx(State::pick_task_file),
        )
        .child(
            tool("quick-task-tags", "Task labels", &icons::TAG)
                .fill(if t.show_labels {
                    c(TINT)
                } else {
                    Color::TRANSPARENT
                })
                .on_press_tx(|s: &mut State, tx| {
                    s.desk.task_entry.show_labels = !s.desk.task_entry.show_labels;
                    if s.desk.task_entry.show_labels {
                        tx.focus_when_shown("quick-task-labels", true)
                    }
                }),
        )
        .child(
            tool(
                "quick-task-local",
                if t.no_sync {
                    "Board only (on)"
                } else {
                    "Board only (off)"
                },
                &icons::CLOUD_OFF,
            )
            .fill(if t.no_sync {
                c(TINT)
            } else {
                Color::TRANSPARENT
            })
            .on_press(|s: &mut State| s.desk.task_entry.no_sync = !s.desk.task_entry.no_sync),
        )
        .child(
            click(
                row().key("new-task").h(28.0).px(4.0).center_y(),
                "Task details, prerequisites and assignee",
            )
            .child(mono("needs, for", MUTED))
            .on_press_tx(|s: &mut State, tx| {
                s.go(Page::NewTask);
                tx.focus_when_shown("task-body", true)
            }),
        )
        .child(el().grow(1.0))
        .child(
            action("quick-task-send", "start task", true)
                .fill(c(if ready(s) { INK } else { MUTED }))
                .min_w(99.0)
                .disabled(!ready(s))
                .on_press(|s: &mut State| s.create_task(None)),
        );
    col().w_full().px(inset).pb(20.0).child(body.child(footer))
}
fn state_mark(t: &Task) -> El {
    let mute = matches!(t.state.as_str(), "waiting" | "blocked" | "canceled");
    let ink = if mute { FAINT } else { INK };
    let mut mark = el()
        .size(11.0, 11.0)
        .radius(5.5)
        .border(1.0, c(ink))
        .center();
    if t.state == "waiting" {
        mark = mark.border_dash(2.0, 1.0);
    }
    if t.state == "claimed" || t.state == "completed" {
        mark = mark.child(el().size(5.0, 5.0).radius(2.5).fill(c(ink)));
    }
    row()
        .center_y()
        .gap(6.0)
        .child(mark)
        .child(label(&t.state, 12.0, 400.0, ink))
}
fn task_row(s: &State, t: &Task, compact: bool) -> El {
    let id = t.id;
    let meta = s.desk.task_rows.get(&id);
    let summary = t
        .text
        .lines()
        .find(|l| !l.trim().is_empty())
        .or_else(|| meta.map(|m| m.files.as_str()))
        .unwrap_or("");
    let mut title = row()
        .grow(1.0)
        .min_w(0.0)
        .gap(6.0)
        .center_y()
        .child(mono(format!("#{id}"), FAINT))
        .child(label(summary, 13.0, 400.0, DIM).min_w(0.0).truncate());
    let mut clock = String::new();
    if let Some(meta) = meta {
        if !compact
            && let Some((k, v)) = meta.labels.iter().find(|(k, _)| k.as_str() != "board.task")
        {
            title = title.child(
                mono(format!("{k}={v}"), MUTED)
                    .px(4.0)
                    .py(1.0)
                    .max_w(106.0)
                    .truncate()
                    .border(1.0, c(EDGE))
                    .fill(c(TINT)),
            );
        }
        clock = messages::relative(&meta.created, chrono::Utc::now());
    }
    let needs = if t.needs.is_empty() {
        String::new()
    } else {
        format!(
            "needs {}",
            t.needs
                .iter()
                .map(|id| s
                    .desk
                    .tasks
                    .iter()
                    .find(|t| t.id == *id)
                    .map_or_else(|| format!("#{id}"), |t| format!("#{id} {}", t.state)))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let info = match t.state.as_str() {
        "waiting" => needs,
        "claimed" => t.holder.as_ref().map_or_else(String::new, |who| {
            let remain = t
                .lease_until
                .map(|n| n.saturating_sub(chrono::Utc::now().timestamp()).max(0));
            remain.map_or_else(
                || who.clone(),
                |secs| format!("{who} · {}m", (secs + 59) / 60),
            )
        }),
        "ready" => t
            .assignee
            .as_ref()
            .map_or_else(String::new, |a| format!("for {a}")),
        _ => t
            .result
            .clone()
            .or_else(|| t.hold_kind.clone())
            .unwrap_or_default(),
    };
    let mut line = row()
        .w_full()
        .h(30.0)
        .center_y()
        .gap(if compact { 8.0 } else { 14.0 })
        .child(state_mark(t).w(if compact { 70.0 } else { 88.0 }))
        .child(title);
    if !compact {
        if !info.is_empty() {
            line = line.child(
                mono(&info, MUTED)
                    .max_w(155.0)
                    .min_w(0.0)
                    .truncate()
                    .help(&info),
            );
        }
        line = line.child(
            mono(format!("#{}", t.channel), FAINT)
                .max_w(145.0)
                .min_w(0.0)
                .truncate(),
        );
    }
    line = line.child(
        row()
            .w(30.0)
            .child(el().grow(1.0))
            .child(mono(clock, FAINT)),
    );
    let mut block = col().key(format!("task-row-{id}")).w_full().child(line);
    if compact && !info.is_empty() {
        block = block.child(mono(&info, MUTED).pl(78.0).w_full().truncate());
    }
    if s.page == Page::Task(id) {
        block = block.fill(c(TINT));
    }
    click(block, summary).on_press(move |s: &mut State| s.go(Page::Task(id)))
}

pub(super) fn new_task(s: &State) -> El {
    let d = &s.desk;
    col()
        .w_full()
        .gap(12.0)
        .child(field(
            "task-channel",
            "Channel",
            &d.task_entry.form.channel,
            |s| &mut s.desk.task_entry.form.channel,
            false,
        ))
        .child(field(
            "task-body",
            "Task",
            &d.task_entry.form.body,
            |s| &mut s.desk.task_entry.form.body,
            true,
        ))
        .child(routing(s, true))
        .child(field(
            "task-labels",
            "Labels (one per line)",
            &d.task_entry.labels,
            |s| &mut s.desk.task_entry.labels,
            true,
        ))
        .child(attachments(s))
        .child(
            row()
                .gap(5.0)
                .child(
                    tool("task-attach", "Attach task file", &icons::PAPERCLIP)
                        .on_press_tx(State::pick_task_file),
                )
                .child(
                    tool("task-local", "Board only", &icons::CLOUD_OFF)
                        .fill(if d.task_entry.no_sync {
                            c(TINT)
                        } else {
                            Color::TRANSPARENT
                        })
                        .on_press(|s: &mut State| {
                            s.desk.task_entry.no_sync = !s.desk.task_entry.no_sync
                        }),
                ),
        )
        .child(
            action("create-task", "Create task", true)
                .disabled(!ready(s))
                .on_press(|s: &mut State| s.create_task(None)),
        )
}
pub(super) fn list(s: &State, compact: bool) -> El {
    let d = &s.desk;
    if d.filtering {
        let f = &d.filter_form;
        return col()
            .w_full()
            .gap(12.0)
            .child(heading(
                "Task filters",
                Some(
                    tool("close-task-filters", "Close filters", &glyphs::CLOSE)
                        .on_press(|s: &mut State| s.desk.filtering = false),
                ),
            ))
            .child(field(
                "criteria-channel",
                "Channel or scope",
                &f.channel,
                |s| &mut s.desk.filter_form.channel,
                false,
            ))
            .child(field(
                "criteria-owner",
                "Owner",
                &f.name,
                |s| &mut s.desk.filter_form.name,
                false,
            ))
            .child(field(
                "criteria-assignee",
                "Assignee",
                &f.value,
                |s| &mut s.desk.filter_form.value,
                false,
            ))
            .child(field(
                "criteria-needs",
                "Needs task ID",
                &f.extra,
                |s| &mut s.desk.filter_form.extra,
                false,
            ))
            .child(field(
                "criteria-labels",
                "Labels (one per line)",
                &f.body,
                |s| &mut s.desk.filter_form.body,
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
                                s.desk.filtering = false;
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
    let mut panel = col().key("task-board").w_full().gap(1.0).child(
        row().w_full().center_y().gap(6.0).pb(6.0).child(seg(
            &crate::state::Desk::TASK_STATES,
            d.task_filter,
            "Task state",
            "task-filter",
            |s, i| {
                s.desk.task_filter = i;
                s.desk.first_page();
                s.refresh();
            },
        )),
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
        panel = panel.child(task_row(s, task, compact));
    }
    panel
}
pub(super) fn banner(s: &State) -> El {
    let Some(t) = &s.desk.selected_task else {
        return empty("Task unavailable");
    };
    let fact = |name: &str, value: &str| {
        row()
            .w_full()
            .center_y()
            .gap(6.0)
            .child(mono(name, FAINT).w(48.0))
            .child(
                label(value, 12.0, 400.0, DIM)
                    .grow(1.0)
                    .min_w(0.0)
                    .truncate()
                    .help(value),
            )
    };
    let mut facts = col()
        .key("task-banner")
        .w_full()
        .gap(4.0)
        .child(state_mark(t).pb(10.0))
        .child(fact("owner", &t.owner));
    if let Some(who) = &t.assignee {
        facts = facts.child(fact("for", who));
    }
    if let Some(who) = &t.holder {
        facts = facts.child(fact(
            "claim",
            &t.lease_until
                .map_or_else(|| who.clone(), |n| format!("{who}  until {}", utc(n))),
        ));
    }
    if !t.needs.is_empty() {
        let mut links = row()
            .w_full()
            .center_y()
            .gap(6.0)
            .child(mono("needs", FAINT).w(48.0));
        for &id in t.needs.iter().take(3) {
            let name = s
                .desk
                .tasks
                .iter()
                .find(|n| n.id == id)
                .map_or_else(|| format!("#{id}"), |n| format!("#{id} {}", n.state));
            links = links.child(
                click(
                    row()
                        .key(format!("task-need-{id}"))
                        .center_y()
                        .h(19.0)
                        .min_w(0.0),
                    &format!("Open prerequisite {name}"),
                )
                .child(label(name, 12.0, 400.0, DIM).min_w(0.0).truncate())
                .on_press(move |s: &mut State| s.go(Page::Task(id))),
            );
        }
        if t.needs.len() > 3 {
            links = links.child(mono(format!("+{}", t.needs.len() - 3), FAINT));
        }
        facts = facts.child(links);
    }
    if let Some(reason) = t.result.as_deref().or(t.hold_kind.as_deref()) {
        facts = facts.child(fact(
            if t.state == "completed" {
                "result"
            } else {
                "reason"
            },
            reason,
        ));
    }
    let mut tools = row().w_full().center_y().flex_wrap().gap(5.0).pt(4.0);
    if t.state == "ready" && t.assignee.as_ref().is_none_or(|a| a == &s.actor) {
        tools = tools.child(
            command(t.id, "claim-task", "claim", TaskCommand::Claim, false)
                .h(26.0)
                .help("Claim for 5 minutes"),
        );
    }
    if s.desk.has_claim(&s.actor, t) {
        tools = tools
            .child(
                command(t.id, "renew-task", "renew", TaskCommand::Renew, false)
                    .h(26.0)
                    .help("Renew for 5 minutes"),
            )
            .child(command(t.id, "release-task", "release", TaskCommand::Release, false).h(26.0));
    }
    facts.child(
        tools.child(
            tool("task-options", "Task actions and routing", &icons::ELLIPSIS)
                .size(28.0, 26.0)
                .fill(c(TINT))
                .on_press(|s: &mut State| s.set_task_options(true)),
        ),
    )
}

pub(super) fn detail(s: &State) -> El {
    let d = &s.desk;
    let Some(t) = &d.selected_task else {
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
                row()
                    .gap(5.0)
                    .child(
                        action("task-thread", "Open thread", false)
                            .on_press(move |s: &mut State| s.go(Page::Thread(id))),
                    )
                    .child(
                        tool("task-options", "Back to task conversation", &glyphs::CLOSE)
                            .on_press(|s: &mut State| s.set_task_options(false)),
                    ),
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
