use super::*;
use crate::state::{FeedKind, SwarmTab, SwarmTarget};
use board_local::swarm::{SwarmEntry, SwarmRun};

fn badge(s: &str, ink: &str, bg: &str) -> El {
    label(s, 12., 550., ink)
        .px(6.)
        .py(2.)
        .radius(3.)
        .fill(c(bg))
}
fn pager(s: &State, runs: bool) -> El {
    let o = &s.desk.swarm;
    let (key, older, prev) = if runs {
        ("swarm-runs", o.older_run, !o.run_starts.is_empty())
    } else {
        ("swarm-history", o.older_post, !o.feed_starts.is_empty())
    };
    if older.is_none() && !prev {
        return el();
    }
    row()
        .w_full()
        .center_y()
        .gap(4.)
        .child(mono(if prev { "History" } else { "Latest" }, MUTED).grow(1.))
        .child(
            tool(&format!("{key}-older"), "Older", &icons::PREVIOUS_FRAME)
                .disabled(older.is_none())
                .on_press(move |s: &mut State| s.swarm_page(runs, true)),
        )
        .child(
            tool(&format!("{key}-newer"), "Newer", &icons::NEXT_FRAME)
                .disabled(!prev)
                .on_press(move |s: &mut State| s.swarm_page(runs, false)),
        )
}
fn runs(s: &State, inset: f32) -> El {
    let mut list = col().w_full().gap(1.).px(inset).py(10.);
    if s.desk.swarm.runs.is_empty() {
        list = list
            .child(label("No visible swarms", 16., 600., INK))
            .child(
                mono("No runs on this board are visible to this profile.", MUTED)
                    .w_full()
                    .wrap_anywhere(),
            );
    }
    for run in &s.desk.swarm.runs {
        let id = run.id;
        let line = col()
            .key(format!("swarm-run-{id}"))
            .w_full()
            .py(8.)
            .gap(4.)
            .child(
                row()
                    .w_full()
                    .gap(8.)
                    .center_y()
                    .child(
                        label(&run.slug, 15., 600., INK)
                            .grow(1.)
                            .min_w(0.)
                            .truncate(),
                    )
                    .child(badge(&run.status, DIM, TINT))
                    .child(icon(&icons::NEXT_FRAME, 15.).color(c(MUTED))),
            )
            .child(
                label(&run.goal, 13., 450., DIM)
                    .w_full()
                    .wrap_anywhere()
                    .max_lines(3),
            )
            .child(
                mono(
                    format!(
                        "{}   #{}   {} workers",
                        run.coordinator_actor, run.channel, run.worker_count
                    ),
                    MUTED,
                )
                .w_full()
                .wrap_anywhere(),
            );
        list = list
            .child(
                click(line, &format!("Open swarm {}", run.slug))
                    .on_press(move |s: &mut State| s.go(Page::Swarm(id))),
            )
            .child(rule());
    }
    col()
        .w_full()
        .grow(1.)
        .min_h(0.)
        .child(scroll("swarm-runs", list))
        .child(pager(s, true).px(inset))
}
fn roster(s: &State, run: &SwarmRun) -> El {
    let o = &s.desk.swarm;
    let mut out = col()
        .key("swarm-roster")
        .w_full()
        .gap(3.)
        .px(10.)
        .py(8.)
        .child(label("Roster", 15., 600., INK))
        .child(
            mono(format!("Coordinator: {}", run.coordinator_actor), MUTED)
                .w_full()
                .wrap_anywhere(),
        );
    if run.workers.is_empty() {
        out = out.child(mono("No workers registered", MUTED).py(8.));
    }
    let retired =
        |w: &board_local::swarm::SwarmWorker| matches!(w.status.as_str(), "done" | "stopped");
    let retired_count = run.workers.iter().filter(|w| retired(w)).count();
    if retired_count > 0 {
        out = out.child(
            workspace::tab(
                "swarm-retired",
                &format!("Retired ({retired_count})"),
                o.retired_open,
            )
            .on_press(|s: &mut State| s.desk.swarm.retired_open = !s.desk.swarm.retired_open),
        );
    }
    for w in run.workers.iter().filter(|w| o.retired_open || !retired(w)) {
        let name = w.name.clone();
        let pick = name.clone();
        let expand = name.clone();
        let selected = o.worker.as_deref() == Some(&name);
        let expanded = o.briefs.contains(&name);
        let off = o.offline.contains(&w.id);
        let state = if off { "Offline waiter" } else { &w.status };
        let mut member = col()
            .key(format!("swarm-member-{name}"))
            .w_full()
            .py(5.)
            .gap(4.)
            .child(
                row()
                    .w_full()
                    .center_y()
                    .gap(4.)
                    .child(
                        click(
                            row()
                                .key(format!("swarm-filter-{name}"))
                                .h(28.)
                                .grow(1.)
                                .min_w(0.)
                                .center_y()
                                .fill(if selected {
                                    c(TINT)
                                } else {
                                    Color::TRANSPARENT
                                }),
                            &format!("Filter feed to {name}"),
                        )
                        .child(
                            label(format!("@{name}"), 13., 600., INK)
                                .min_w(0.)
                                .truncate(),
                        )
                        .on_press(move |s: &mut State| {
                            s.desk.swarm.worker = (!selected).then(|| pick.clone());
                            s.desk.swarm.roster_open = false;
                            s.refresh_swarm_filter();
                        }),
                    )
                    .child(
                        tool(
                            &format!("swarm-brief-{name}"),
                            if expanded {
                                "Hide public brief"
                            } else {
                                "Show public brief"
                            },
                            if expanded {
                                &glyphs::CHEVRON_UP
                            } else {
                                &glyphs::CHEVRON_DOWN
                            },
                        )
                        .expanded(expanded)
                        .on_press(move |s: &mut State| {
                            if !s.desk.swarm.briefs.remove(&expand) {
                                s.desk.swarm.briefs.insert(expand.clone());
                            }
                        }),
                    ),
            )
            .child(
                row()
                    .w_full()
                    .gap(6.)
                    .center_y()
                    .flex_wrap()
                    .child(badge(w.role_slug.as_deref().unwrap_or("worker"), DIM, TINT))
                    .child(
                        mono(state, if off { ERROR } else { MUTED })
                            .key(format!("swarm-state-{name}")),
                    ),
            );
        member = member.child(
            mono(
                w.last_activity_at
                    .as_ref()
                    .map(|at| {
                        format!(
                            "Last swarm activity {}",
                            messages::relative(at, chrono::Utc::now())
                        )
                    })
                    .unwrap_or_else(|| "No worker activity recorded".into()),
                FAINT,
            )
            .w_full()
            .wrap_anywhere(),
        );
        if let Some(error) = &w.launch_error {
            member = member.child(label(error, 12., 400., ERROR).w_full().wrap_anywhere());
        }
        if expanded {
            member = member
                .child(
                    label(&w.brief, 13., 400., DIM)
                        .w_full()
                        .wrap_anywhere()
                        .selectable(true),
                )
                .child(
                    mono(&w.actor, MUTED)
                        .w_full()
                        .wrap_anywhere()
                        .selectable(true),
                );
            for (key, val) in [
                ("Branch", w.branch_name.as_deref()),
                ("Workspace", w.workspace_path.as_deref()),
                ("Reported model", w.model.as_deref()),
                ("Workspace mode", w.workspace_mode.as_deref()),
                ("Session reference", w.session_ref.as_deref()),
            ] {
                if let Some(v) = val {
                    member = member.child(
                        mono(format!("{key}: {v}"), MUTED)
                            .w_full()
                            .wrap_anywhere()
                            .selectable(true),
                    );
                }
            }
            if let Some(v) = w.pinned_role_version {
                member = member.child(mono(format!("Role version {v}"), MUTED));
            }
        }
        out = out.child(rule()).child(member);
    }
    out
}
fn entry(s: &State, e: &SwarmEntry) -> El {
    let mut out = col()
        .key(format!("swarm-entry-{}", e.cursor))
        .w_full()
        .py(7.)
        .gap(5.);
    if let Some(dm) = &e.dm {
        let sender = s
            .desk
            .swarm
            .run
            .as_ref()
            .and_then(|r| {
                if r.coordinator_actor == dm.from_actor {
                    Some("coordinator")
                } else {
                    r.workers
                        .iter()
                        .find(|w| w.actor == dm.from_actor)
                        .map(|w| w.name.as_str())
                }
            })
            .unwrap_or(&dm.from_actor);
        out = out
            .child(
                row()
                    .w_full()
                    .center_y()
                    .gap(6.)
                    .flex_wrap()
                    .child(badge("DM", INK, TINT))
                    .child(
                        label(format!("@{sender} → @{}", dm.to_worker), 13., 600., INK)
                            .wrap_anywhere()
                            .help(&dm.from_actor),
                    )
                    .child(mono(stamp(&dm.created_at), MUTED)),
            )
            .child(
                label(&dm.text, 14., 400., DIM)
                    .w_full()
                    .wrap_anywhere()
                    .selectable(true),
            );
    } else if let Some(m) = &e.message {
        let id = m.id;
        out = out
            .child(
                row()
                    .w_full()
                    .center_y()
                    .gap(7.)
                    .child(
                        label(&m.actor, 13., 600., INK)
                            .grow(1.)
                            .min_w(0.)
                            .truncate(),
                    )
                    .child(mono(stamp(&m.created_at), MUTED))
                    .child(
                        click(
                            mono(format!("#{id}"), MUTED)
                                .key(format!("swarm-message-{id}"))
                                .px(4.)
                                .py(3.),
                            "Open board thread",
                        )
                        .on_press(move |s: &mut State| s.open_message_thread(id)),
                    ),
            )
            .child(rich::scoped_body(s, m, "swarm-"));
        if !e.notified.is_empty() {
            out = out.child(
                mono(
                    format!(
                        "Notified {}",
                        e.notified
                            .iter()
                            .map(|n| format!("@{n}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    MUTED,
                )
                .w_full()
                .wrap_anywhere(),
            );
        }
    }
    out
}
fn feed(s: &State) -> El {
    let o = &s.desk.swarm;
    let mut filters = row().w_full().gap(10.).center_y().flex_wrap();
    for (key, title, kind) in [
        ("all", "All", FeedKind::All),
        ("public", "Public", FeedKind::Public),
        ("dm", "DMs", FeedKind::Direct),
        ("addressed", "To me", FeedKind::Addressed),
    ] {
        filters = filters.child(
            workspace::tab(&format!("swarm-kind-{key}"), title, o.kind == kind).on_press(
                move |s: &mut State| {
                    s.desk.swarm.kind = kind;
                    s.refresh_swarm_filter();
                },
            ),
        );
    }
    if let Some(w) = &o.worker {
        filters = filters.child(
            action("swarm-clear-worker", &format!("@{w} ×"), false).on_press(|s: &mut State| {
                s.desk.swarm.worker = None;
                s.refresh_swarm_filter();
            }),
        );
    }
    let mut list = col().w_full().px(10.);
    let mut any = false;
    let mut new_divider = false;
    let unfiltered = matches!(o.kind, FeedKind::All | FeedKind::Public) && o.worker.is_none();
    if unfiltered
        && let Some(last) = o
            .entries
            .iter()
            .rev()
            .find(|e| e.message.is_some() && e.cursor > o.public_seen)
    {
        let through = last.cursor;
        filters = filters.child(
            tool(
                "swarm-mark-read",
                "Mark public posts read through this page",
                &glyphs::CHECK,
            )
            .on_press(move |s: &mut State| s.mark_swarm_seen(through)),
        );
    }
    for e in &o.entries {
        if unfiltered && !new_divider && e.message.is_some() && e.cursor > o.public_seen {
            list = list.child(
                mono("New public posts", MUTED)
                    .key("swarm-public-new")
                    .py(4.),
            );
            new_divider = true;
        }
        list = list.child(entry(s, e)).child(rule());
        any = true;
    }
    if !any {
        list = list.child(mono("No matching entries", MUTED).py(8.));
    }
    col()
        .w_full()
        .grow(1.)
        .min_h(0.)
        .child(filters.px(10.))
        .child(pager(s, false).px(10.))
        .child(
            col()
                .key("swarm-feed")
                .w_full()
                .grow(1.)
                .min_h(0.)
                .scroll(ScrollSpec {
                    stick_bottom: o.feed_starts.is_empty(),
                })
                .child(list),
        )
}
fn overview(s: &State) -> El {
    let o = &s.desk.swarm;
    let Some(view) = &o.overview else { return el() };
    let mut body = col().w_full().px(10.).py(8.).gap(5.);
    let link = |key: String, title: String, id: i64| {
        click(
            label(&title, 13., 500., INK)
                .key(key)
                .w_full()
                .wrap_anywhere()
                .py(3.),
            &title,
        )
        .on_press(move |s: &mut State| s.open_message_thread(id))
    };
    let pages = |tasks: bool, back: bool, next: Option<i64>| {
        let prefix = if tasks {
            "swarm-threads"
        } else {
            "swarm-actions"
        };
        if !back && next.is_none() {
            return el();
        }
        row()
            .gap(4.)
            .child(
                tool(
                    &format!("{prefix}-back"),
                    "Previous page",
                    &icons::PREVIOUS_FRAME,
                )
                .disabled(!back)
                .on_press(move |s: &mut State| {
                    let o = &mut s.desk.swarm;
                    if tasks {
                        o.task_starts.pop();
                    } else {
                        o.action_starts.pop();
                    }
                    s.refresh();
                }),
            )
            .child(
                tool(&format!("{prefix}-next"), "Next page", &icons::NEXT_FRAME)
                    .disabled(next.is_none())
                    .on_press(move |s: &mut State| {
                        if let Some(at) = next {
                            let o = &mut s.desk.swarm;
                            if tasks {
                                o.task_starts.push(at)
                            } else {
                                o.action_starts.push(at)
                            }
                            s.refresh();
                        }
                    }),
            )
    };
    body = body.child(label("Needs attention", 15., 600., INK));
    if view.actions.is_empty() {
        body = body.child(mono("No open actions on this page", MUTED));
    }
    for a in &view.actions {
        body = body.child(link(
            format!("swarm-action-{}", a.cursor),
            format!(
                "{} · {} · {}",
                a.assigned_to,
                a.kind.replace('_', " "),
                a.summary
            ),
            a.message_id,
        ));
    }
    body = body
        .child(pages(
            false,
            !o.action_starts.is_empty(),
            view.next_action_after,
        ))
        .child(rule())
        .child(label("Task threads", 15., 600., INK));
    if view.threads.is_empty() {
        body = body.child(mono("No task threads on this page", MUTED));
    }
    let mut groups = std::collections::BTreeMap::new();
    for t in &view.threads {
        groups
            .entry(t.workstream.as_deref().unwrap_or("Ungrouped"))
            .or_insert_with(Vec::new)
            .push(t);
    }
    for (group, tasks) in groups {
        body = body.child(label(group, 13., 600., DIM).py(3.).wrap_anywhere());
        for t in tasks {
            let owner = t
                .claimant
                .as_deref()
                .or(t.assignee.as_deref())
                .unwrap_or("Unassigned");
            body = body
                .child(link(
                    format!("swarm-task-{}", t.id),
                    format!("#{}  {}", t.id, t.title),
                    t.id,
                ))
                .child(mono(format!("{} · {owner}", t.state), MUTED).wrap_anywhere());
            for (name, id) in [
                ("Handoff", t.latest_handoff),
                ("Verification", t.latest_check),
            ] {
                if let Some(id) = id {
                    body = body.child(link(
                        format!("swarm-task-{}-{name}", t.id),
                        format!("{name} #{id}"),
                        id,
                    ));
                }
            }
        }
    }
    scroll(
        "swarm-overview-scroll",
        body.child(pages(true, !o.task_starts.is_empty(), view.next_task_after)),
    )
}
fn composer(s: &State, r: &SwarmRun, overlays: &mut Vec<El>) -> El {
    let o = &s.desk.swarm;
    let options = std::iter::once("Post to board".to_owned())
        .chain(std::iter::once("DM @coordinator".into()))
        .chain(r.workers.iter().map(|w| format!("DM @{}", w.name)))
        .collect::<Vec<_>>();
    let target = match &o.target {
        SwarmTarget::Public => options[0].clone(),
        SwarmTarget::Direct(w) => format!("DM @{w}"),
    };
    let picker = rune_kit::Select::new("swarm-target", &options)
        .current(Some(&target))
        .label("Swarm delivery target")
        .placement(rune_kit::Placement::TopStart)
        .width(195.)
        .open(o.choose_target)
        .on_toggle(|s: &mut State| s.desk.swarm.choose_target = !s.desk.swarm.choose_target)
        .on_close(|s: &mut State| s.desk.swarm.choose_target = false)
        .on_choose(|s: &mut State, v, tx| {
            s.desk.swarm.target = match v.strip_prefix("DM @") {
                Some(w) => SwarmTarget::Direct(w.into()),
                None => SwarmTarget::Public,
            };
            s.desk.swarm.choose_target = false;
            tx.focus_when_shown("swarm-body", true);
        })
        .build(&Kit::neutral(dark()), overlays);
    let editor = text_input(&o.body, font(14., 400.).lh(20.))
        .key("swarm-body")
        .label("Swarm message")
        .w_full()
        .h(54.)
        .padding(8.)
        .color(c(INK))
        .placeholder(
            if matches!(o.target, SwarmTarget::Public) {
                "Post to this swarm…"
            } else {
                "Direct message…"
            },
            c(FAINT),
        )
        .on_edit(|s: &mut State| &mut s.desk.swarm.body)
        .on_input_key(|s: &mut State, k, tx| {
            if k.key == Key::Enter && !k.mods.shift && s.desk.swarm.body.preedit().is_none() {
                s.swarm_send(tx);
                true
            } else {
                false
            }
        });
    col()
        .key("swarm-composer")
        .w_full()
        .border(1., c(CONTROL))
        .border_dash(3., 2.)
        .child(editor)
        .child(
            row()
                .w_full()
                .px(7.)
                .pb(6.)
                .center_y()
                .gap(4.)
                .child(picker)
                .child(el().grow(1.))
                .child(
                    submit("swarm-send", "Send", !o.body.text.trim().is_empty())
                        .on_press_tx(State::swarm_send),
                ),
        )
}
pub(super) fn view(s: &State, inset: f32, narrow: bool) -> El {
    let o = &s.desk.swarm;
    let Some(run) = &o.run else {
        return runs(s, inset);
    };
    let mut overlays = Vec::new();
    let options = o.runs.iter().map(|r| r.slug.as_str()).collect::<Vec<_>>();
    let picker = rune_kit::Select::new("swarm-select", &options)
        .label("Select swarm run")
        .current(Some(&run.slug))
        .open(o.choose_run)
        .placement(rune_kit::Placement::BottomStart)
        .width(205.)
        .on_toggle(|s: &mut State| s.desk.swarm.choose_run = !s.desk.swarm.choose_run)
        .on_close(|s: &mut State| s.desk.swarm.choose_run = false)
        .on_choose(|s: &mut State, slug, _| {
            s.desk.swarm.choose_run = false;
            if let Some(id) = s
                .desk
                .swarm
                .runs
                .iter()
                .find(|r| r.slug == slug)
                .map(|r| r.id)
            {
                s.go(Page::Swarm(id));
            }
        })
        .build(&Kit::neutral(dark()), &mut overlays);
    let mut head = col()
        .w_full()
        .px(inset)
        .py(6.)
        .gap(5.)
        .child(
            row()
                .w_full()
                .center_y()
                .gap(7.)
                .child(
                    tool("swarm-runs-back", "All swarms", &icons::PREVIOUS_FRAME)
                        .on_press(|s: &mut State| s.go(Page::Swarms)),
                )
                .child(picker.max_w(210.).min_w(0.))
                .child(el().grow(1.))
                .child(badge(&run.status, DIM, TINT)),
        )
        .child(
            label(&run.goal, 14., 500., DIM)
                .w_full()
                .wrap_anywhere()
                .max_lines(3)
                .help(&run.goal)
                .selectable(true),
        );
    let mut meta = row().w_full().center_y().gap(7.).flex_wrap().child(
        mono(
            format!(
                "#{} · {} entries · {} workers",
                run.channel,
                run.activity_count,
                run.workers.len()
            ),
            MUTED,
        )
        .wrap_anywhere(),
    );
    if !o.offline.is_empty() {
        meta = meta.child(badge(
            &format!("{} offline", o.offline.len()),
            DEL_INK,
            DEL_BG,
        ));
    }
    if run.status == "active" && run.notifications_sent >= run.max_notifications {
        meta = meta.child(badge("Notification limit reached", DEL_INK, DEL_BG));
    }
    meta = meta.child(el().grow(1.));
    if run.status == "active" && o.can_send && run.can_control {
        if o.confirm_stop {
            meta = meta
                .child(
                    action("swarm-keep", "Keep running", false)
                        .on_press(|s: &mut State| s.desk.swarm.confirm_stop = false),
                )
                .child(
                    action("swarm-stop-confirm", "Confirm stop", true).on_press(State::swarm_stop),
                );
        } else {
            meta = meta.child(action("swarm-stop", "Stop run", false).on_press(State::swarm_stop));
        }
    }
    head = head.child(meta).child(attention::banner(s, Some(run.id)));
    let tabs = row()
        .w_full()
        .gap(12.)
        .flex_wrap()
        .px(10.)
        .child(
            workspace::tab("swarm-overview", "Overview", o.tab == SwarmTab::Overview)
                .on_press(|s: &mut State| s.swarm_tab(SwarmTab::Overview)),
        )
        .child(
            workspace::tab(
                "swarm-conversation",
                "Conversation",
                o.tab == SwarmTab::Conversation,
            )
            .on_press(|s: &mut State| s.swarm_tab(SwarmTab::Conversation)),
        )
        .child(
            workspace::tab("swarm-receipt", "Receipt", o.tab == SwarmTab::Receipt)
                .on_press(|s: &mut State| s.swarm_tab(SwarmTab::Receipt)),
        );
    let mut stream = col()
        .grow(1.)
        .min_w(0.)
        .min_h(0.)
        .h_full()
        .child(tabs)
        .child(match o.tab {
            SwarmTab::Receipt => attention::receipt(s),
            SwarmTab::Conversation => feed(s),
            SwarmTab::Overview => overview(s),
        });
    if o.can_send && o.tab == SwarmTab::Conversation {
        stream = stream.child(col().w_full().px(10.).pb(10.).child(composer(
            s,
            run,
            &mut overlays,
        )));
    }
    let body = if narrow {
        let row = row()
            .w_full()
            .px(inset)
            .gap(12.)
            .child(
                workspace::tab("swarm-feed-tab", "Activity", !o.roster_open)
                    .on_press(|s: &mut State| s.desk.swarm.roster_open = false),
            )
            .child(
                workspace::tab("swarm-roster-tab", "Roster", o.roster_open)
                    .on_press(|s: &mut State| s.desk.swarm.roster_open = true),
            );
        col()
            .w_full()
            .grow(1.)
            .min_h(0.)
            .child(row)
            .child(if o.roster_open {
                scroll("swarm-roster-scroll", roster(s, run))
            } else {
                stream
            })
    } else {
        row()
            .w_full()
            .grow(1.)
            .min_h(0.)
            .child(
                col()
                    .w(218.)
                    .min_w(218.)
                    .h_full()
                    .border_sides([0., 1., 0., 0.], c(EDGE))
                    .child(scroll("swarm-roster-scroll", roster(s, run))),
            )
            .child(stream)
    };
    let mut panel = col()
        .key("swarm-observer")
        .w_full()
        .grow(1.)
        .min_h(0.)
        .child(head)
        .child(rule())
        .child(body);
    for overlay in overlays {
        panel = panel.child(overlay);
    }
    panel
}
