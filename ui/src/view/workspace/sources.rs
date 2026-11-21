use super::*;
use board_local::sources::{GithubItems, Provider, Source};
use rune_kit::Checkbox;

fn provider(p: &Provider) -> &'static str {
    match p {
        Provider::Local => "Local",
        Provider::Github => "GitHub",
        Provider::Slack => "Slack",
        Provider::Linear => "Linear",
    }
}
fn note(text: &str) -> El {
    label(text, 12.0, 400.0, MUTED).w_full().wrap_anywhere()
}
fn error(s: &State, panel: El) -> El {
    if let Some(e) = &s.desk.source.notice {
        panel.child(
            label(e, 13.0, 500.0, INK)
                .key("source-error")
                .w_full()
                .wrap_anywhere(),
        )
    } else {
        panel
    }
}
fn create(s: &State) -> El {
    let f = &s.desk.source.mapping;
    let mut providers = row()
        .label("Source provider")
        .w_full()
        .center_y()
        .gap(14.)
        .flex_wrap()
        .focus_group();
    for (key, p) in [
        ("local", Provider::Local),
        ("github", Provider::Github),
        ("slack", Provider::Slack),
        ("linear", Provider::Linear),
    ] {
        providers = providers.child(
            tab(
                &format!("source-provider-{key}"),
                provider(&p),
                p == f.provider,
            )
            .on_press(move |s: &mut State| {
                if p != s.desk.source.mapping.provider {
                    s.desk.source.mapping.provider = p.clone();
                    s.desk.source.mapping.remote = TextEdit::new("");
                    s.desk.source.notice = None;
                }
            }),
        );
    }
    let destination = match f.provider {
        Provider::Github => "GitHub repository (owner/repo)",
        Provider::Slack => "Slack conversation ID",
        Provider::Linear => "Linear team key",
        Provider::Local => "External stream ID",
    };
    let pane = col()
        .w_full()
        .max_w(560.)
        .gap(10.)
        .child(heading("New source", None))
        .child(providers)
        .child(field(
            "source-name",
            "Source name",
            &f.name,
            |s| &mut s.desk.source.mapping.name,
            false,
        ))
        .child(field(
            "source-remote",
            destination,
            &f.remote,
            |s| &mut s.desk.source.mapping.remote,
            false,
        ))
        .child(field(
            "source-channel",
            "Board channel",
            &f.channel,
            |s| &mut s.desk.source.mapping.channel,
            false,
        ))
        .child(note(
            "Publishing is off by default. Creating a mapping does not contact the provider.",
        ));
    let ready = [&f.name, &f.remote, &f.channel]
        .iter()
        .all(|v| !v.text.trim().is_empty());
    error(s, pane).child(
        row()
            .w_full()
            .center_y()
            .gap(8.)
            .child(
                action("source-create", "Create source", ready)
                    .disabled(!ready)
                    .on_press(State::create_source_mapping),
            )
            .child(action("source-cancel", "Cancel", false).on_press(State::cancel_source_form)),
    )
}
fn toggle(key: &str, title: &str, checked: bool, field: fn(&mut Source) -> &mut bool) -> El {
    Checkbox::new(checked)
        .label(title)
        .on_toggle(move |s: &mut State| {
            if let Some(e) = s.desk.source.edit.as_mut() {
                let value = field(&mut e.draft);
                *value = !*value;
                e.confirm = false;
            }
        })
        .build(&Kit::neutral(dark()))
        .key(key)
}
fn settings(s: &State) -> El {
    let e = s.desk.source.edit.as_ref().expect("source settings opened");
    let d = &e.draft;
    let mut pane = col()
        .w_full()
        .max_w(560.)
        .gap(10.)
        .child(heading("Source settings", None))
        .child(
            mono(
                format!(
                    "{}    {}    #{}",
                    provider(&d.provider),
                    d.remote,
                    d.channel
                ),
                DIM,
            )
            .w_full()
            .wrap_anywhere(),
        );
    if e.confirm {
        if e.original.outbound != d.outbound {
            pane=pane.child(note(if d.outbound {
                "Enable publishing? Future Board changes can enter this source's outbox. Only an explicitly run sender publishes them."
            } else {
                "Disable publishing? Unsent deliveries for this source will be skipped. A request already in flight cannot be recalled."
            }));
        }
        if e.original.settings.mirror_files && !d.settings.mirror_files {
            pane=pane.child(note("Disabling file mirroring skips unsent file uploads. Existing copies are not removed."));
        }
        return error(s, pane).child(
            row()
                .w_full()
                .center_y()
                .gap(8.)
                .child(
                    action("source-settings-confirm", "Confirm changes", true)
                        .on_press(State::save_source_settings),
                )
                .child(
                    action("source-settings-back", "Back", false).on_press(|s: &mut State| {
                        if let Some(e) = s.desk.source.edit.as_mut() {
                            e.confirm = false;
                        }
                    }),
                ),
        );
    }
    pane = pane.child(toggle(
        "source-publish",
        "Allow outbound publishing",
        d.outbound,
        |s| &mut s.outbound,
    ));
    if d.provider != Provider::Github {
        pane = pane.child(toggle(
            "source-mirror-roots",
            "Mirror new Board threads",
            d.settings.mirror_roots,
            |s| &mut s.settings.mirror_roots,
        ));
    } else {
        let mut choices = row().w_full().center_y().gap(14.).flex_wrap().focus_group();
        for (key, title, item) in [
            ("all", "Both", GithubItems::All),
            ("issues", "Issues", GithubItems::Issues),
            ("pulls", "Pull requests", GithubItems::Pulls),
        ] {
            choices = choices.child(
                tab(
                    &format!("source-gh-{key}"),
                    title,
                    d.settings.github_items == item,
                )
                .on_press(move |s: &mut State| {
                    if let Some(e) = s.desk.source.edit.as_mut() {
                        e.draft.settings.github_items = item;
                    }
                }),
            );
        }
        pane = pane.child(note("Incoming GitHub items")).child(choices);
    }
    if d.provider == Provider::Slack {
        pane = pane.child(toggle(
            "source-mirror-files",
            "Mirror attached files",
            d.settings.mirror_files,
            |s| &mut s.settings.mirror_files,
        ));
    }
    if d.provider != Provider::Local {
        pane = pane.child(toggle(
            "source-display-authors",
            "Include author bylines",
            d.settings.display_authors,
            |s| &mut s.settings.display_authors,
        ));
    }
    error(s, pane).child(
        row()
            .w_full()
            .center_y()
            .gap(8.)
            .child(
                action(
                    "source-settings-save",
                    "Save settings",
                    e.original != e.draft,
                )
                .disabled(e.original == e.draft)
                .on_press(State::save_source_settings),
            )
            .child(
                action("source-settings-cancel", "Cancel", false)
                    .on_press(State::cancel_source_form),
            ),
    )
}

pub(super) fn view(s: &State) -> El {
    let source = &s.desk.source;
    if s.page == Page::Sources {
        if source.creating {
            return create(s);
        }
        let add = source.can_add.then(|| {
            action("source-add", "Add source", true).on_press_tx(|s: &mut State, tx| {
                s.begin_source();
                tx.focus_when_shown("source-name", true);
            })
        });
        let mut rows = col().w_full().gap(5.0).child(heading("Sources", add));
        if source.list.is_empty() {
            rows = rows.child(empty(if source.can_add {"No sources configured for this profile."} else {"No sources available. Source settings require read and sync permission in the same channel."}));
        }
        for item in &source.list {
            let key = item.name.clone();
            rows = rows.child(listrow(
                &format!("source-{}", item.name),
                &item.name,
                &format!(
                    "{}    #{}    {}",
                    provider(&item.provider),
                    item.channel,
                    if item.outbound {
                        "Publishing on"
                    } else {
                        "Publishing off"
                    }
                ),
                move |s| {
                    s.desk.source.selected = key.clone();
                    s.desk.source.outgoing = false;
                    s.go(Page::Inbox);
                },
            ));
        }
        return rows;
    }
    let Some(config) = source.list.first() else {
        return empty("Source unavailable");
    };
    if source.edit.is_some() {
        return settings(s);
    }
    let mut panel = col()
        .w_full()
        .gap(5.0)
        .child(heading(
            &config.name,
            Some(
                action("source-list", "all sources", false)
                    .on_press(|s: &mut State| s.go(Page::Sources)),
            ),
        ))
        .child(
            mono(
                format!(
                    "{}    {}    #{}    Owner: {}",
                    provider(&config.provider),
                    config.remote,
                    config.channel,
                    config.owner
                ),
                MUTED,
            )
            .w_full()
            .wrap_anywhere(),
        )
        .child(heading(
            if config.outbound {
                "Publishing on"
            } else {
                "Publishing off"
            },
            Some(
                action("source-configure", "Settings", false).on_press(State::edit_source_settings),
            ),
        ))
        .child(if source.can_copy_command {
            row()
                .w_full()
                .center_y()
                .gap(8.)
                .child(
                    mono(source.command(), DIM)
                        .key("source-command")
                        .grow(1.)
                        .min_w(0.)
                        .wrap_anywhere(),
                )
                .child(
                    tool(
                        "source-copy-command",
                        if cfg!(windows) {
                            "Copy PowerShell command for this board"
                        } else {
                            "Copy command for this board"
                        },
                        &glyphs::COPY,
                    )
                    .on_press_tx(State::copy_source_command),
                )
        } else {
            note("Command unavailable for this database path.")
        })
        .child(seg(
            &["Inbox", "Outbox"],
            usize::from(source.outgoing),
            "Delivery queue",
            "source-queue",
            |s, i| {
                s.desk.source.outgoing = i == 1;
                s.desk.source.expanded = None;
                s.desk.first_page();
                s.refresh();
            },
        ));
    if source.rows.is_empty() {
        panel = panel.child(empty("No deliveries on this page"));
    }
    for job in &source.rows {
        let id = job.id;
        panel = panel.child(listrow(
            &format!("job-{id}"),
            &format!("#{id}    {}", job.operation),
            &format!("{}    Attempts: {}", job.state, job.attempts),
            move |s| {
                s.desk.source.expanded = (s.desk.source.expanded != Some(id)).then_some(id);
                s.refresh();
            },
        ));
        if Some(id) == source.expanded {
            if job.state == "uncertain" {
                panel = panel.child(label("External outcome unknown", 12.0, 500.0, ACCENT));
            } else if job.error_recorded {
                panel = panel.child(label("Delivery error recorded", 12.0, 500.0, ACCENT));
            }
            if let Some(local) = job.local {
                panel = panel.child(
                    row()
                        .w_full()
                        .gap(8.0)
                        .center_y()
                        .child(
                            mono(format!("Linked copies: {}", source.copies), MUTED)
                                .grow(1.0)
                                .min_w(0.0)
                                .truncate(),
                        )
                        .child(
                            action(
                                &format!("job-message-{id}"),
                                &format!("Open #{local}"),
                                false,
                            )
                            .on_press(move |s: &mut State| s.open_message_thread(local)),
                        ),
                );
            }
        }
    }
    panel
}
