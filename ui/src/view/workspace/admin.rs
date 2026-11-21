use super::*;
use crate::state::SchemaPart;

fn open_form(s: &mut State) {
    if s.page == Page::Workspace && s.desk.access.registry() {
        s.go(Page::Access);
    }
    s.desk.enter(s.page);
    s.desk.editing = true;
    match s.page {
        Page::Schemas => {
            s.desk.form.value = TextEdit::new("1");
            s.desk.form.body = TextEdit::new("{\n  \"type\": \"object\",\n  \"properties\": {}\n}");
        }
        Page::Schedules => {
            if let Some(ts) = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|d| chrono::DateTime::from_timestamp(d.as_secs() as i64 + 3600, 0))
            {
                s.desk.form.name = TextEdit::new(ts.to_rfc3339());
            }
        }
        _ => (),
    }
    s.refresh();
}
pub(super) fn add_resource(s: &State) -> Option<El> {
    if s.desk.editing {
        return None;
    }
    let kind = match s.page {
        Page::Workspace | Page::Access if s.desk.access.registry() => "grant",
        Page::Schemas => "schema",
        Page::Schedules => "schedule",
        Page::Watchers => "watcher",
        _ => return None,
    };
    Some(
        click(
            row()
                .key("add-resource")
                .h(25.0)
                .center_y()
                .px(8.0)
                .gap(5.0)
                .fill(c(TINT)),
            &format!("Add {kind}"),
        )
        .child(icon(&icons::PLUS, 12.0).color(c(DIM)))
        .child(label(kind, 12.0, 400.0, DIM))
        .on_press(open_form),
    )
}
fn form(s: &State) -> El {
    let d = &s.desk;
    let mut fields = col().w_full().gap(11.0);
    let submit = match s.page {
        Page::Schedules => {
            fields = fields
                .child(field(
                    "resource-channel",
                    "Channel or #message ID",
                    &d.form.channel,
                    |s| &mut s.desk.form.channel,
                    false,
                ))
                .child(field(
                    "resource-name",
                    "First send (RFC 3339)",
                    &d.form.name,
                    |s| &mut s.desk.form.name,
                    false,
                ))
                .child(field(
                    "resource-body",
                    "Message",
                    &d.form.body,
                    |s| &mut s.desk.form.body,
                    true,
                ))
                .child(field(
                    "resource-value",
                    "Repeat seconds (optional)",
                    &d.form.value,
                    |s| &mut s.desk.form.value,
                    false,
                ))
                .child(field(
                    "resource-extra",
                    "Maximum sends (optional)",
                    &d.form.extra,
                    |s| &mut s.desk.form.extra,
                    false,
                ));
            "Schedule"
        }
        Page::Watchers => {
            fields = fields
                .child(field(
                    "resource-name",
                    "Watcher name",
                    &d.form.name,
                    |s| &mut s.desk.form.name,
                    false,
                ))
                .child(field(
                    "resource-channel",
                    "Channel or scope (optional)",
                    &d.form.channel,
                    |s| &mut s.desk.form.channel,
                    false,
                ))
                .child(field(
                    "resource-value",
                    "Event type (optional)",
                    &d.form.value,
                    |s| &mut s.desk.form.value,
                    false,
                ));
            "Add watcher"
        }
        Page::Access => {
            fields = fields
                .child(field(
                    "resource-name",
                    "Person or profile",
                    &d.form.name,
                    |s| &mut s.desk.form.name,
                    false,
                ))
                .child(field(
                    "resource-body",
                    "Channel permissions",
                    &d.form.body,
                    |s| &mut s.desk.form.body,
                    true,
                ));
            "Create grant"
        }
        _ => {
            fields = fields
                .child(field(
                    "resource-name",
                    "Schema name",
                    &d.form.name,
                    |s| &mut s.desk.form.name,
                    false,
                ))
                .child(field(
                    "resource-value",
                    "Version",
                    &d.form.value,
                    |s| &mut s.desk.form.value,
                    false,
                ))
                .child(field(
                    "resource-extra",
                    "Description",
                    &d.form.extra,
                    |s| &mut s.desk.form.extra,
                    false,
                ))
                .child(field(
                    "resource-body",
                    "JSON Schema",
                    &d.form.body,
                    |s| &mut s.desk.form.body,
                    true,
                ));
            "Register"
        }
    };
    fields.child(
        row()
            .w_full()
            .gap(6.0)
            .child(action("save-resource", submit, true).on_press(State::save_admin))
            .child(
                action("close-form", "Cancel", false).on_press(|s: &mut State| {
                    s.desk.editing = false;
                    s.error = None;
                }),
            ),
    )
}
fn remove(s: &State, key: String, verb: &str) -> El {
    if s.desk.confirm.as_deref() == Some(&key) {
        row()
            .gap(6.0)
            .child(
                action(&format!("confirm-{key}"), &format!("Confirm {verb}"), true)
                    .on_press(move |s: &mut State| s.remove_admin(&key)),
            )
            .child(
                action("keep-resource", "Keep", false)
                    .on_press(|s: &mut State| s.desk.confirm = None),
            )
    } else {
        action(&format!("remove-{key}"), &format!("{verb}..."), false)
            .on_press(move |s: &mut State| s.desk.confirm = Some(key.clone()))
    }
}
fn section(name: &str, detail: &str) -> El {
    row()
        .w_full()
        .center_y()
        .gap(12.0)
        .py(3.0)
        .child(
            label(name, 12.0, 400.0, DIM)
                .grow(1.0)
                .min_w(0.0)
                .wrap_anywhere(),
        )
        .child(
            label(detail, 12.0, 400.0, MUTED)
                .grow(1.5)
                .min_w(0.0)
                .wrap_anywhere()
                .selectable(true),
        )
}

fn actions<'a>(values: impl IntoIterator<Item = &'a board_local::access::Action>) -> String {
    use board_local::access::Action::*;
    let values = values
        .into_iter()
        .map(|v| match v {
            Read => "read",
            Post => "post",
            React => "react",
            Label => "label",
            Sync => "sync",
            Manage => "manage",
        })
        .collect::<Vec<_>>();
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(" ")
    }
}
fn scope(rules: &board_local::access::Rules) -> String {
    rules
        .0
        .iter()
        .map(|(channel, rights)| format!("{channel}  {}", actions(rights)))
        .collect::<Vec<_>>()
        .join("    ")
}
fn access_subject(s: &mut State, other: bool, focus_actor: bool, tx: &mut Tx) {
    if other && !s.desk.access.registry() {
        return;
    }
    let review = &mut s.desk.access;
    review.other = other;
    review.query = None;
    review.decision = None;
    let key = if other && focus_actor {
        "explain-actor"
    } else if other {
        "explain-subject"
    } else {
        "explain-self"
    };
    tx.focus_when_shown(key, true);
}

fn subject_switch(other: bool) -> El {
    let mut group = row()
        .key("explain-identity")
        .role("radiogroup")
        .label("Check permissions for")
        .focus_group()
        .h(30.)
        .padding(2.)
        .radius(5.)
        .fill(c(TINT))
        .border(1., c(CONTROL));
    for (index, key, name) in [(0, "explain-self", "You"), (1, "explain-subject", "Actor")] {
        let actor = index == 1;
        let on = actor == other;
        let mut segment = click(row().key(key).w(61.).h(24.).center().radius(3.), name)
            .role("radio")
            .checked(on)
            .focus_selected(on)
            .position_in_set(index + 1, 2);
        if on {
            segment = segment.fill(c(BG)).border(1., c(MUTED));
        }
        group = group.child(
            segment
                .child(label(
                    name,
                    13.,
                    if on { 600. } else { 450. },
                    if on { INK } else { MUTED },
                ))
                .on_press_tx(move |s: &mut State, tx| access_subject(s, actor, true, tx))
                .on_key(|s: &mut State, k, tx| match k.key {
                    Key::Left | Key::Right => {
                        access_subject(s, !s.desk.access.other, false, tx);
                        true
                    }
                    _ => false,
                }),
        );
    }
    group
}

fn explainer(s: &State) -> El {
    let a = &s.desk.access;
    let enabled = !a.fields.channel.text.trim().is_empty()
        && (!a.other || !a.fields.name.text.trim().is_empty());
    let check = click(
        row()
            .key("explain-access")
            .h(30.)
            .px(12.)
            .gap(7.)
            .center()
            .radius(4.)
            .border(1., c(if enabled { INK } else { CONTROL }))
            .fill(c(if enabled { INK } else { BG })),
        "Explain access",
    )
    .child(icon(&glyphs::SEARCH, 13.).color(c(if enabled { BG } else { FAINT })))
    .child(label(
        "Explain",
        13.,
        550.,
        if enabled { BG } else { FAINT },
    ))
    .disabled(!enabled)
    .on_press(State::explain_access);
    let input = |edit: &TextEdit,
                 key: &'static str,
                 title: &str,
                 prompt: &str,
                 prefix: Option<&str>,
                 write: fn(&mut State) -> &mut TextEdit| {
        let editor = short_input(edit, key, title, prompt)
            .on_edit(write)
            .on_input_key(|s: &mut State, k, _| {
                if k.key == Key::Enter
                    && s.desk.access.fields.channel.preedit().is_none()
                    && s.desk.access.fields.name.preedit().is_none()
                {
                    s.explain_access();
                    true
                } else {
                    false
                }
            });
        short_field(key, edit, prefix, editor).w(174.)
    };
    let labeled = |name: &str, child: El| {
        col()
            .gap(5.)
            .child(label(name, 12., 500., MUTED))
            .child(child)
    };
    let mut out =
        col()
            .w_full()
            .gap(10.0)
            .pt(10.0)
            .child(label("Check permissions", 15., 600., INK));
    let mut controls = row()
        .w_full()
        .gap(12.0)
        .flex_wrap()
        .center_y()
        .child(labeled(
            "Channel",
            input(
                &a.fields.channel,
                "explain-channel",
                "Channel to explain",
                "channel",
                Some("#"),
                |s| &mut s.desk.access.fields.channel,
            ),
        ));
    if a.registry() {
        controls = controls.child(labeled("For", subject_switch(a.other)));
    }
    if a.other && a.registry() {
        controls = controls.child(labeled(
            "Actor",
            input(
                &a.fields.name,
                "explain-actor",
                "Person or profile to explain",
                "name or profile",
                None,
                |s| &mut s.desk.access.fields.name,
            ),
        ));
    }
    out = out.child(controls.child(col().h(50.).justify_end().child(check)));
    if let (Some((who, ch, _)), Some(d)) = (&a.query, &a.decision) {
        let mut result = format!("@{who}  #{ch}  {}", actions(&d.actions));
        if let Some(level) = &d.level {
            result.push_str(&format!("  ·  {level}"));
        }
        if let Some(pattern) = &d.pattern {
            result.push_str(&format!("  {pattern}"));
        }
        if !d.grants.is_empty() {
            result.push_str(&format!(
                "  grants {}",
                d.grants
                    .iter()
                    .map(|id| format!("#{id}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        out = out.child(
            mono(result, DIM)
                .key("access-explanation")
                .w_full()
                .wrap_anywhere()
                .selectable(true),
        );
    }
    out
}
fn overview(s: &State) -> El {
    let d = &s.desk;
    let mut list = col()
        .key("local-board")
        .w_full()
        .gap(3.0)
        .child(mono("local board", FAINT))
        .child(section(
            "connection",
            if s.connected() {
                "Local SQLite"
            } else {
                "Unavailable"
            },
        ))
        .child(section(
            "snapshot",
            &format!("Event {}   /   {} channels", d.head, s.channels.len()),
        ));
    if !s.delegated {
        list = list.child(section(
            "database",
            &local::database_path().display().to_string(),
        ));
    }
    list
}
fn profiles(s: &State) -> El {
    let mut panel = col()
        .w_full()
        .gap(12.0)
        .child(heading(
            "Profiles",
            Some(
                action("open-directory", "People", false)
                    .on_press(|s: &mut State| s.go(Page::People)),
            ),
        ))
        .child(section("current", &s.actor));
    for (i, who) in s.desk.profiles.iter().enumerate() {
        let actor = who.clone();
        panel = panel.child(
            listrow(
                &format!("profile-{i}"),
                who,
                if who == &s.actor {
                    "Current"
                } else {
                    "Switch profile"
                },
                move |s| s.switch_profile(&actor),
            )
            .disabled(s.delegated || who == &s.actor),
        );
    }
    if s.delegated {
        return panel.child(empty("Identity fixed by local credential"));
    }
    panel
        .child(field(
            "profile-name",
            "New or existing profile name",
            &s.desk.form.name,
            |s| &mut s.desk.form.name,
            false,
        ))
        .child(
            action("switch-profile", "Use profile", true)
                .disabled(s.desk.form.name.text.trim().is_empty())
                .on_press(|s: &mut State| {
                    s.switch_profile(&format!(
                        "{}/{}",
                        local::person_name(),
                        s.desk.form.name.text.trim()
                    ))
                }),
        )
}

pub(super) fn view(s: &State) -> El {
    if s.page == Page::Profiles {
        return profiles(s);
    }
    let d = &s.desk;
    let mut out = col().w_full().gap(8.0);
    if d.editing {
        return out.child(form(s));
    }
    match s.page {
        Page::Schedules => {
            if d.schedules.is_empty() {
                out = out.child(empty("No schedules"));
            }
            for sched in &d.schedules {
                let mut item = col()
                    .key(format!("schedule-{}", sched.id))
                    .w_full()
                    .gap(3.0)
                    .child(
                        row()
                            .w_full()
                            .gap(8.0)
                            .center_y()
                            .child(
                                mono(
                                    format!(
                                        "#{}    {}    #{}",
                                        sched.id, sched.state, sched.channel
                                    ),
                                    MUTED,
                                )
                                .max_w(190.0)
                                .wrap_anywhere(),
                            )
                            .child(
                                label(&sched.text, 12.5, 450.0, INK)
                                    .grow(1.0)
                                    .min_w(0.0)
                                    .wrap_anywhere(),
                            ),
                    )
                    .child(
                        mono(
                            format!(
                                "{}    {}    sent {}{}",
                                sched.actor,
                                utc(sched.due),
                                sched.sent,
                                if sched.no_sync { "    board only" } else { "" }
                            ),
                            MUTED,
                        )
                        .w_full()
                        .wrap_anywhere(),
                    );
                if let Some(n) = sched.every {
                    let stop = sched
                        .max_sends
                        .map(|n| format!("up to {n} sends"))
                        .or_else(|| sched.until.map(|n| format!("until {}", utc(n))))
                        .unwrap_or_else(|| "until canceled".into());
                    item = item.child(mono(format!("Every {n}s, {stop}"), MUTED));
                }
                if let Some(end) = &sched.canceled {
                    item = item.child(
                        mono(format!("{}: {}", end.by, end.reason), MUTED)
                            .w_full()
                            .wrap_anywhere(),
                    );
                }
                if let Some(reply) = sched.re {
                    item = item.child(
                        action(
                            &format!("schedule-target-{}", sched.id),
                            &format!("Reply to #{reply}"),
                            false,
                        )
                        .on_press(move |s: &mut State| {
                            // A scheduled reply can point at a non-root message.
                            s.open_message_thread(reply);
                        }),
                    );
                }
                if sched.state == "pending" && board_local::owned_by(&sched.actor, &s.actor) {
                    item = item.child(remove(s, sched.id.to_string(), "Cancel"));
                }
                out = out.child(item).child(rule());
            }
        }
        Page::Watchers => {
            if d.watchers.is_empty() {
                out = out.child(empty("No watchers"));
            }
            for w in &d.watchers {
                let mut criteria =
                    serde_json::to_value(&w.filter).expect("serializable stored filter");
                if let Some(obj) = criteria.as_object_mut() {
                    obj.retain(|_, v| {
                        !v.is_null()
                            && !v.as_object().is_some_and(|o| o.is_empty())
                            && !v.as_array().is_some_and(|a| a.is_empty())
                    });
                }
                out = out
                    .child(
                        col()
                            .key(format!("watcher-{}", w.name))
                            .w_full()
                            .gap(3.0)
                            .child(
                                row()
                                    .w_full()
                                    .gap(8.0)
                                    .center_y()
                                    .child(
                                        label(&w.name, 13.0, 500.0, INK)
                                            .grow(1.0)
                                            .min_w(0.0)
                                            .wrap_anywhere(),
                                    )
                                    .child(
                                        mono(
                                            format!(
                                                "Cursor {}    {}",
                                                w.cursor,
                                                w.run_as.as_deref().unwrap_or("Local")
                                            ),
                                            MUTED,
                                        )
                                        .wrap_anywhere(),
                                    )
                                    .child(remove(s, w.name.clone(), "Remove")),
                            )
                            .child(
                                mono(
                                    format!(
                                        "{}    {}",
                                        w.kind.as_deref().unwrap_or("All event types"),
                                        if criteria.as_object().is_some_and(|o| o.is_empty()) {
                                            "All channels".into()
                                        } else {
                                            criteria.to_string()
                                        }
                                    ),
                                    MUTED,
                                )
                                .w_full()
                                .wrap_anywhere()
                                .selectable(true),
                            ),
                    )
                    .child(rule());
            }
        }
        Page::Schemas => {
            for schema in &d.schemas {
                let name = schema.name.clone();
                let version = schema.version;
                let page = d.schema_open.as_ref().filter(|p| p.id == schema.id);
                let open = page.is_some();
                out = out.child(listrow(
                    &format!("schema-{}-{}", schema.name, schema.version),
                    &format!("{}@{}", schema.name, schema.version),
                    &schema.description,
                    move |s| {
                        if open {
                            s.desk.schema_open = None;
                        } else {
                            s.show_schema(&name, version);
                        }
                    },
                ));
                if let Some(page) = page {
                    let copy = |key, part, name| {
                        tool(
                            key,
                            name,
                            if page.copied == Some(part) {
                                &glyphs::CHECK
                            } else {
                                &glyphs::COPY
                            },
                        )
                        .on_press_tx(move |s: &mut State, tx| s.copy_schema_part(part, tx))
                    };
                    let mut detail = col().w_full().gap(8.0).py(8.0);
                    if let Some(example) = &page.example {
                        detail = detail
                            .child(
                                row()
                                    .w_full()
                                    .center_y()
                                    .gap(6.0)
                                    .child(label("Example block", 15.0, 600.0, INK).grow(1.0))
                                    .child(copy(
                                        "copy-schema-example",
                                        SchemaPart::Example,
                                        "Copy example block",
                                    )),
                            )
                            .child(
                                mono(example, INK)
                                    .key("schema-example")
                                    .w_full()
                                    .wrap_anywhere()
                                    .selectable(true),
                            )
                            .child(
                                action("compose-schema-example", "Add example to draft", false)
                                    .on_press_tx(State::compose_example),
                            );
                    } else {
                        detail = detail.child(
                            label("No validated example available", 14.0, 500.0, MUTED)
                                .wrap_anywhere(),
                        );
                    }
                    out = out.child(
                        detail
                            .child(
                                row()
                                    .w_full()
                                    .center_y()
                                    .gap(6.0)
                                    .child(label("Schema definition", 15.0, 600.0, INK).grow(1.0))
                                    .child(copy(
                                        "copy-schema-definition",
                                        SchemaPart::Definition,
                                        "Copy schema definition",
                                    )),
                            )
                            .child(
                                mono(&page.definition, INK)
                                    .w_full()
                                    .wrap_anywhere()
                                    .selectable(true),
                            ),
                    );
                }
            }
            if d.schemas.is_empty() {
                out = out.child(empty("No schemas"));
            }
        }
        Page::Access | Page::Workspace => {
            out = out.child(mono("you", FAINT));
            if let Some(v) = &d.access.viewer {
                out = out.child(section(&v.actor, &scope(&v.can)));
            }
            out = out.child(explainer(s));
            if !d.access.registry() {
                return out.child(overview(s).pt(12.0));
            }
            out = out.child(mono("grants", FAINT).pt(8.0));
            if d.grants.is_empty() {
                out = out.child(empty("No grants"));
            }
            for g in &d.grants {
                out = out.child(
                    row()
                        .w_full()
                        .center_y()
                        .gap(6.0)
                        .child(
                            section(
                                &format!("Grant #{}    {}", g.id, g.subjects.join(", ")),
                                &scope(&g.can),
                            )
                            .min_w(0.0)
                            .grow(1.0),
                        )
                        .child(remove(s, format!("grant-{}", g.id), "Revoke")),
                );
            }
            if s.delegated {
                return out.child(overview(s).pt(12.0));
            }
            out = out.child(mono("credentials", FAINT).pt(8.0));
            if d.credentials.is_empty() {
                out = out.child(empty("No credentials"));
            }
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            for c in &d.credentials {
                let state = if c.revoked {
                    "Revoked"
                } else if c.expires.is_some_and(|t| t <= now) {
                    "Expired"
                } else {
                    "Active"
                };
                out = out.child(section(&format!("{state}    {}", c.actor), &c.id));
                if let Some(rules) = &c.scope {
                    out = out.child(mono(scope(rules), MUTED).w_full().wrap_anywhere());
                }
                if let Some(expires) = c.expires {
                    out = out.child(mono(format!("Expires {}", utc(expires)), MUTED));
                }
                if !c.revoked {
                    out = out.child(remove(s, format!("credential-{}", c.id), "Revoke"));
                }
                out = out.child(rule());
            }
            if s.page == Page::Workspace {
                out = out.child(overview(s).pt(12.0));
            }
        }
        _ => (),
    }
    out
}
