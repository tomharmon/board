use super::*;
use crate::state::{RoleConfirm, RoleMode};
use workspace::{field, tab};

fn heading(title: &str) -> El {
    label(title, 16., 600., INK).w_full().wrap_anywhere()
}
fn line(
    key: &str,
    title: &str,
    sub: &str,
    tag: Option<&str>,
    press: impl Fn(&mut State) + 'static,
) -> El {
    let mut top = row().w_full().center_y().gap(8.).child(
        label(title, 14., 550., INK)
            .grow(1.)
            .min_w(0.)
            .wrap_anywhere(),
    );
    if let Some(tag) = tag {
        top = top.child(mono(tag, MUTED).px(5.).py(2.).fill(c(TINT)));
    }
    click(col().key(key).w_full().py(6.).gap(3.), title)
        .child(top)
        .child(mono(sub, MUTED).w_full().wrap_anywhere())
        .on_press(press)
}
fn importing(s: &State) -> El {
    let l = &s.desk.roles;
    if !l.importing {
        return action("role-import-folder", "Import skill folder", false).on_press_tx(
            |s: &mut State, tx| {
                s.desk.roles.importing = true;
                s.desk.roles.attaching = false;
                tx.focus_when_shown("skill-path", true);
            },
        );
    }
    col()
        .key("skill-import")
        .w_full()
        .gap(5.)
        .child(field(
            "skill-path",
            "Local skill folder",
            &l.path,
            |s| &mut s.desk.roles.path,
            false,
        ))
        .child(
            row()
                .w_full()
                .gap(5.)
                .center_y()
                .flex_wrap()
                .child(
                    action("skill-browse-path", "Choose folder…", false)
                        .on_press_tx(State::choose_skill_dir),
                )
                .child(
                    action("skill-import-cancel", "Cancel", false)
                        .on_press(|s: &mut State| s.desk.roles.cancel()),
                )
                .child(
                    submit(
                        "skill-import-save",
                        if l.mode == RoleMode::Edit {
                            "Import and attach"
                        } else {
                            "Import"
                        },
                        !l.path.text.trim().is_empty(),
                    )
                    .on_press(State::import_skill),
                ),
        )
}
fn confirmation(s: &State) -> El {
    let Some(c) = &s.desk.roles.confirm else {
        return el();
    };
    let (question, verb) = match c {
        RoleConfirm::DeleteRole => ("Delete this custom role?".to_string(), "Delete role"),
        RoleConfirm::Reset => (
            "Replace this role's prompt with its built-in default?".into(),
            "Restore default",
        ),
        RoleConfirm::DeleteSkill(slug, roles) => (
            if roles.is_empty() {
                format!("Delete {slug} from the skill library?")
            } else {
                format!(
                    "Delete {slug} from the library and remove it from these roles: {}?",
                    roles.join(", ")
                )
            },
            "Delete from library",
        ),
    };
    col()
        .key("role-confirmation")
        .w_full()
        .gap(5.)
        .py(5.)
        .child(label(question, 13., 600., INK).w_full().wrap_anywhere())
        .child(
            row()
                .w_full()
                .flex_wrap()
                .gap(6.)
                .child(
                    action("role-keep", "Cancel", false)
                        .on_press(|s: &mut State| s.desk.roles.confirm = None),
                )
                .child(action("role-confirm", verb, true).on_press(State::confirm_role_change)),
        )
}
fn role_list(s: &State) -> El {
    let mut out = col().w_full().gap(3.).child(
        row()
            .w_full()
            .center_y()
            .gap(7.)
            .child(heading("Role templates").grow(1.).min_w(0.))
            .child(
                action("role-new", "New role", true).on_press_tx(|s: &mut State, tx| {
                    s.edit_role(None);
                    tx.focus_when_shown("role-slug", true);
                }),
            ),
    );
    for r in &s.desk.roles.roles {
        let slug = r.slug.clone();
        out = out
            .child(line(
                &format!("role-{slug}"),
                &r.name,
                &r.description,
                r.is_builtin.then_some("Built-in"),
                move |s| s.edit_role(Some(&slug)),
            ))
            .child(rule());
    }
    out
}
fn editor(s: &State) -> El {
    let l = &s.desk.roles;
    let ed = &l.editor;
    let original = ed.original.as_ref();
    let builtin = original.is_some_and(|r| r.is_builtin);
    let mut form = col()
        .w_full()
        .gap(9.)
        .child(heading(original.map_or("New role", |r| r.name.as_str())));
    if let Some(r) = original {
        form = form.child(
            mono(
                format!(
                    "{}   {}   Version {}",
                    if builtin { "Built-in" } else { "Custom" },
                    r.slug,
                    r.version
                ),
                MUTED,
            )
            .w_full()
            .wrap_anywhere(),
        );
        if l.roles
            .iter()
            .find(|row| row.slug == r.slug)
            .is_none_or(|now| now.version != r.version)
        {
            let slug = r.slug.clone();
            form = form.child(
                row()
                    .w_full()
                    .gap(5.)
                    .center_y()
                    .flex_wrap()
                    .child(label("Role changed outside this editor", 13., 500., ERROR))
                    .child(
                        action("role-reload", "Reload", false)
                            .on_press(move |s: &mut State| s.edit_role(Some(&slug))),
                    ),
            );
        }
    }
    if !builtin {
        form = form.child(field(
            "role-slug",
            "Slug",
            &ed.slug,
            |s| &mut s.desk.roles.editor.slug,
            false,
        ));
    }
    form = form
        .child(field(
            "role-name",
            "Name",
            &ed.name,
            |s| &mut s.desk.roles.editor.name,
            false,
        ))
        .child(field(
            "role-description",
            "Description",
            &ed.description,
            |s| &mut s.desk.roles.editor.description,
            false,
        ))
        .child(field(
            "role-prompt",
            "System prompt",
            &ed.prompt,
            |s| &mut s.desk.roles.editor.prompt,
            true,
        ));
    let mut buttons = row().w_full().gap(6.).center_y().flex_wrap().child(
        submit(
            "role-save",
            "Save role",
            ![&ed.slug, &ed.name, &ed.prompt]
                .iter()
                .any(|f| f.text.trim().is_empty()),
        )
        .on_press(State::save_role),
    );
    if original.is_some() {
        buttons = buttons.child(if builtin {
            action("role-reset", "Restore default", false)
                .on_press(|s: &mut State| s.desk.roles.confirm = Some(RoleConfirm::Reset))
        } else {
            action("role-delete", "Delete role", false)
                .on_press(|s: &mut State| s.desk.roles.confirm = Some(RoleConfirm::DeleteRole))
        });
    }
    form = form.child(buttons).child(confirmation(s));
    let Some(r) = original else { return form };
    form = form.child(rule()).child(heading("Attached skills"));
    let skills = l
        .roles
        .iter()
        .find(|row| row.slug == r.slug)
        .map_or(&r.skills, |r| &r.skills);
    for slug in skills {
        let name = slug.clone();
        form = form.child(
            row()
                .w_full()
                .center_y()
                .gap(7.)
                .flex_wrap()
                .child(mono(slug, INK).grow(1.).min_w(0.).wrap_anywhere())
                .child(
                    action(
                        &format!("role-detach-{slug}"),
                        "Remove from this role",
                        false,
                    )
                    .on_press(move |s: &mut State| s.role_skill(&name, false)),
                ),
        );
    }
    if skills.is_empty() {
        form = form.child(mono("No attached skills", MUTED));
    }
    form = form.child(row().w_full().child(
        action("role-attach-skill", "Attach existing skill", false).on_press(|s: &mut State| {
            s.desk.roles.attaching = !s.desk.roles.attaching;
            s.desk.roles.importing = false;
        }),
    ));
    if l.attaching {
        let mut any = false;
        for sk in l.skills.iter().filter(|sk| !skills.contains(&sk.slug)) {
            let name = sk.slug.clone();
            form = form.child(line(
                &format!("role-attach-{name}"),
                &sk.name,
                &sk.slug,
                None,
                move |s| s.role_skill(&name, true),
            ));
            any = true;
        }
        if !any {
            form = form.child(mono("All library skills are already attached", MUTED));
        }
    }
    form.child(importing(s))
}
fn skills(s: &State) -> El {
    let l = &s.desk.roles;
    let mut out = col()
        .w_full()
        .gap(6.)
        .child(heading("Skill library"))
        .child(importing(s));
    for sk in &l.skills {
        let name = sk.slug.clone();
        out = out.child(rule()).child(line(
            &format!("skill-{name}"),
            &sk.name,
            &sk.slug,
            sk.is_builtin.then_some("Built-in"),
            move |s| s.show_skill(&name),
        ));
        if let Some(bundle) = l.skill.as_ref().filter(|b| b.info.slug == sk.slug) {
            out = out
                .child(mono(&sk.description, DIM).w_full().wrap_anywhere())
                .child(
                    mono(
                        format!(
                            "Version {}   Roles: {}",
                            sk.version,
                            if sk.roles.is_empty() {
                                "none".into()
                            } else {
                                sk.roles.join(", ")
                            }
                        ),
                        MUTED,
                    )
                    .w_full()
                    .wrap_anywhere(),
                )
                .child(
                    mono(
                        format!(
                            "Files: {}",
                            bundle.files.keys().cloned().collect::<Vec<_>>().join(", ")
                        ),
                        MUTED,
                    )
                    .w_full()
                    .wrap_anywhere(),
                );
            if let Some(md) = bundle.files.get("SKILL.md") {
                out = out.child(
                    mono(md, INK)
                        .key("skill-document")
                        .w_full()
                        .wrap_anywhere()
                        .max_lines(12)
                        .selectable(true),
                );
            }
            if !sk.is_builtin {
                let slug = sk.slug.clone();
                out = out
                    .child(
                        action("skill-delete", "Delete from library", false)
                            .on_press(move |s: &mut State| s.ask_delete_skill(&slug)),
                    )
                    .child(confirmation(s));
            }
        }
    }
    out
}
pub(super) fn view(s: &State) -> El {
    if s.delegated {
        return mono("Agent Roles requires the local board owner.", MUTED)
            .w_full()
            .wrap_anywhere();
    }
    let mode = s.desk.roles.mode;
    col()
        .key("agent-roles")
        .w_full()
        .gap(8.)
        .child(
            row()
                .w_full()
                .gap(14.)
                .center_y()
                .child(
                    tab("roles-tab", "Role templates", mode != RoleMode::Skills)
                        .on_press(|s: &mut State| s.desk.roles.set_mode(RoleMode::Roles)),
                )
                .child(
                    tab("skills-tab", "Skill library", mode == RoleMode::Skills)
                        .on_press(|s: &mut State| s.desk.roles.set_mode(RoleMode::Skills)),
                ),
        )
        .child(match mode {
            RoleMode::Roles => role_list(s),
            RoleMode::Edit => editor(s),
            RoleMode::Skills => skills(s),
        })
}
