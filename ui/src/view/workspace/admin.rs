use super::*;

fn open_form(s: &mut State) {
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
    col().w_full().gap(5.0).child(mono(name, MUTED)).child(
        label(detail, 14.0, 510.0, INK)
            .w_full()
            .wrap_anywhere()
            .selectable(true),
    )
}
fn overview(s: &State) -> El {
    let d = &s.desk;
    let mut list = col()
        .key("work-sections")
        .w_full()
        .gap(10.0)
        .child(heading("Workspace", None))
        .child(section(
            "DATABASE",
            &local::database_path().display().to_string(),
        ))
        .child(section(
            "CONNECTION",
            if s.connected() {
                "Local SQLite"
            } else {
                "Unavailable"
            },
        ))
        .child(section(
            "SNAPSHOT",
            &format!("Event {}   /   {} channels", d.head, s.channels.len()),
        ))
        .child(rule());
    for (name, meta, key, page) in [
        (
            "Schedules",
            "Pending and previous sends",
            "work-schedules",
            Page::Schedules,
        ),
        (
            "Watchers",
            "Subscriptions and acknowledgement cursors",
            "work-watchers",
            Page::Watchers,
        ),
        (
            "Schemas",
            "Versioned local definitions",
            "work-schemas",
            Page::Schemas,
        ),
        (
            "Profiles",
            "Local person and agent identities",
            "work-profiles",
            Page::Profiles,
        ),
    ] {
        list = list.child(listrow(key, name, meta, move |s| s.go(page)));
    }
    list = list.child(listrow(
        "work-sources",
        "Sources",
        "Local mappings and delivery queues",
        |s| s.go(Page::Sources),
    ));
    if !s.delegated {
        list = list.child(listrow(
            "work-access",
            "Access",
            "Grants and local credentials",
            |s| s.go(Page::Access),
        ));
    }
    list
}
fn profiles(s: &State) -> El {
    let mut panel = col()
        .w_full()
        .gap(12.0)
        .child(heading("Profiles", None))
        .child(section("CURRENT", &s.actor));
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
    if s.page == Page::Workspace {
        return overview(s);
    }
    if s.page == Page::Profiles {
        return profiles(s);
    }
    let d = &s.desk;
    let title = name(s.page, s);
    let button = (!d.editing).then(|| {
        action(
            "add-resource",
            match s.page {
                Page::Schedules => "Schedule",
                Page::Watchers => "Add watcher",
                Page::Access => "Add grant",
                _ => "Register",
            },
            true,
        )
        .on_press(open_form)
    });
    let mut out = col().w_full().gap(13.0).child(heading(&title, button));
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
                    .gap(6.0)
                    .child(
                        mono(
                            format!("#{}    {}    #{}", sched.id, sched.state, sched.channel),
                            ACCENT,
                        )
                        .w_full()
                        .wrap_anywhere(),
                    )
                    .child(
                        label(&sched.text, 15.0, 550.0, INK)
                            .w_full()
                            .wrap_anywhere(),
                    )
                    .child(
                        mono(
                            format!(
                                "{}    {}    Sent {}",
                                sched.actor,
                                utc(sched.due),
                                sched.sent
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
                out = out
                    .child(
                        col()
                            .key(format!("watcher-{}", w.name))
                            .w_full()
                            .gap(6.0)
                            .child(label(&w.name, 16.0, 600.0, INK))
                            .child(
                                mono(
                                    format!(
                                        "Cursor {}    {}",
                                        w.cursor,
                                        w.run_as.as_deref().unwrap_or("Local")
                                    ),
                                    MUTED,
                                )
                                .w_full()
                                .wrap_anywhere(),
                            )
                            .child(
                                mono(
                                    format!(
                                        "{}    {}",
                                        w.kind.as_deref().unwrap_or("All event types"),
                                        w.filter.channel.as_deref().unwrap_or("All channels")
                                    ),
                                    MUTED,
                                )
                                .w_full()
                                .wrap_anywhere(),
                            )
                            .child(
                                mono(
                                    serde_json::to_string(&w.filter)
                                        .expect("serializable stored filter"),
                                    MUTED,
                                )
                                .w_full()
                                .wrap_anywhere()
                                .selectable(true),
                            )
                            .child(remove(s, w.name.clone(), "Remove")),
                    )
                    .child(rule());
            }
        }
        Page::Schemas => {
            for schema in &d.schemas {
                let id = (schema.name.clone(), schema.version);
                let open = d.schema_open.as_ref() == Some(&id);
                out = out.child(listrow(
                    &format!("schema-{}-{}", schema.name, schema.version),
                    &format!("{}@{}", schema.name, schema.version),
                    &schema.description,
                    move |s| {
                        s.desk.schema_open = (!open).then(|| id.clone());
                    },
                ));
                if open {
                    out = out.child(
                        mono(
                            serde_json::to_string_pretty(&schema.schema).expect("valid schema"),
                            INK,
                        )
                        .w_full()
                        .wrap_anywhere()
                        .selectable(true),
                    );
                }
            }
            if d.schemas.is_empty() {
                out = out.child(empty("No schemas"));
            }
        }
        Page::Access => {
            out = out.child(mono("GRANTS", MUTED));
            if d.grants.is_empty() {
                out = out.child(empty("No grants"));
            }
            for g in &d.grants {
                out = out
                    .child(section(
                        &format!("Grant #{}    {}", g.id, g.subject),
                        &serde_json::to_string(&g.can).expect("valid policy"),
                    ))
                    .child(remove(s, format!("grant-{}", g.id), "Revoke"))
                    .child(rule());
            }
            out = out.child(mono("CREDENTIALS", MUTED));
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
                    out = out.child(
                        mono(serde_json::to_string(rules).expect("valid scope"), MUTED)
                            .w_full()
                            .wrap_anywhere(),
                    );
                }
                if let Some(expires) = c.expires {
                    out = out.child(mono(format!("Expires {}", utc(expires)), MUTED));
                }
                if !c.revoked {
                    out = out.child(remove(s, format!("credential-{}", c.id), "Revoke"));
                }
                out = out.child(rule());
            }
        }
        _ => (),
    }
    out
}
