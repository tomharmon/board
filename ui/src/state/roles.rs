use super::{Page, State};
use anyhow::{Context, Result, ensure};
use board_local::{
    Board,
    roles::{Role, SkillBundle, SkillSummary},
};
use rune::{FileDialogOptions, TextEdit, Tx};
use std::{path::Path, sync::Arc};

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum RoleMode {
    #[default]
    Roles,
    Edit,
    Skills,
}
pub enum RoleConfirm {
    Reset,
    DeleteRole,
    DeleteSkill(String, Vec<String>),
}
#[derive(Default)]
pub struct Editor {
    pub original: Option<Role>,
    pub name: TextEdit,
    pub slug: TextEdit,
    pub description: TextEdit,
    pub prompt: TextEdit,
}
impl From<Role> for Editor {
    fn from(r: Role) -> Self {
        Self {
            name: TextEdit::new(&r.name),
            slug: TextEdit::new(&r.slug),
            description: TextEdit::new(&r.description),
            prompt: TextEdit::new(&r.system_prompt),
            original: Some(r),
        }
    }
}
#[derive(Default)]
pub struct Library {
    pub mode: RoleMode,
    pub roles: Vec<Role>,
    pub skills: Vec<SkillSummary>,
    pub skill: Option<SkillBundle>,
    pub editor: Editor,
    pub attaching: bool,
    pub importing: bool,
    pub path: TextEdit,
    pub confirm: Option<RoleConfirm>,
    picker: Arc<()>,
}
impl Library {
    pub fn load(&mut self, db: &Board) -> Result<()> {
        self.roles = db.roles_list()?;
        self.skills = db.skills_list()?;
        if let Some(slug) = self.skill.as_ref().map(|s| s.info.slug.clone()) {
            self.skill = if self.skills.iter().any(|s| s.slug == slug) {
                Some(db.skill_show(&slug)?)
            } else {
                None
            };
        }
        Ok(())
    }
    pub fn cancel(&mut self) {
        self.confirm = None;
        self.importing = false;
        self.attaching = false;
        self.path.clear();
        self.picker = Arc::default();
    }
    pub fn set_mode(&mut self, m: RoleMode) {
        self.cancel();
        self.mode = m;
        self.skill = None;
    }
}
impl State {
    fn role_db(&self) -> Result<&Board> {
        ensure!(
            self.page == Page::Roles && !self.delegated,
            "Role settings require the local board owner"
        );
        self.db()
    }
    pub fn edit_role(&mut self, slug: Option<&str>) {
        let result = self
            .role_db()
            .and_then(|db| slug.map(|s| db.role_show(s)).transpose());
        if let Some(role) = self.result(result) {
            let l = &mut self.desk.roles;
            l.set_mode(RoleMode::Edit);
            l.editor = role.map(Editor::from).unwrap_or_default();
        }
    }
    pub fn save_role(&mut self) {
        let result = self.role_db().and_then(|db| {
            let ed = &self.desk.roles.editor;
            let writer = db.role_library_as(&self.actor);
            let (slug, name, desc, prompt) = (
                ed.slug.text.trim(),
                ed.name.text.trim(),
                ed.description.text.trim(),
                ed.prompt.text.as_str(),
            );
            match &ed.original {
                None => writer.role_create(slug, name, desc, prompt),
                Some(old) => {
                    writer.role_edit(&old.slug, slug, name, desc, prompt, Some(old.version))
                }
            }
        });
        if let Some(role) = self.result(result) {
            self.desk.roles.editor = Editor::from(role);
            self.desk.roles.cancel();
            self.refresh();
        }
    }
    pub fn role_skill(&mut self, skill: &str, attach: bool) {
        let result = self.role_db().and_then(|db| {
            let writer = db.role_library_as(&self.actor);
            let r = self
                .desk
                .roles
                .editor
                .original
                .as_ref()
                .context("Save the role first")?;
            if attach {
                writer.role_attach_skill(&r.slug, skill)
            } else {
                writer.role_detach_skill(&r.slug, skill)
            }
        });
        if let Some(role) = self.result(result) {
            self.refresh_role_links(role);
        }
    }
    fn refresh_role_links(&mut self, role: Role) {
        // Do not overwrite an unsaved prompt when changing a skill link; do not
        // advance a stale prompt's expected version past an external edit either.
        let editor = &mut self.desk.roles.editor;
        if editor.original.as_ref().is_some_and(|r| {
            r.system_prompt == role.system_prompt
                && r.name == role.name
                && r.description == role.description
                && r.slug == role.slug
        }) {
            editor.original = Some(role);
        }
        self.desk.roles.cancel();
        self.refresh();
    }
    pub fn choose_skill_dir(&mut self, tx: &mut Tx) {
        if self.role_db().is_err() || !self.desk.roles.importing {
            return;
        }
        let stamp = Arc::downgrade(&self.desk.roles.picker);
        tx.open_folder(
            FileDialogOptions {
                title: "Import skill folder".into(),
                ..Default::default()
            },
            move |s: &mut State, result, _| {
                if stamp.upgrade().is_none() || !s.desk.roles.importing || s.role_db().is_err() {
                    return;
                }
                match result {
                    Ok(Some(p)) => match p.to_str() {
                        Some(v) => s.desk.roles.path = TextEdit::new(v),
                        None => s.error = Some("Folder path is not UTF-8".into()),
                    },
                    Ok(None) => (),
                    Err(e) => s.error = Some(e.to_string()),
                }
            },
        );
    }
    pub fn import_skill(&mut self) {
        let result = self.role_db().and_then(|db| {
            let l = &self.desk.roles;
            let writer = db.role_library_as(&self.actor);
            ensure!(
                l.importing && !l.path.text.trim().is_empty(),
                "Select a skill folder"
            );
            let target = if l.mode == RoleMode::Edit {
                let target = l.editor.original.as_ref().context("Save the role first")?;
                let current = db.role_show(&target.slug)?;
                ensure!(
                    current.version == target.version,
                    "Role changed; reload before importing"
                );
                Some(current.slug)
            } else {
                None
            };
            let bundle = writer.skill_import(Path::new(l.path.text.trim()))?;
            let role = target
                .map(|r| {
                    writer
                        .role_attach_skill(&r, &bundle.info.slug)
                        .context("Skill imported into the library but not attached to this role")
                })
                .transpose()?;
            Ok((bundle, role))
        });
        match self.result(result) {
            Some((_, Some(role))) => self.refresh_role_links(role),
            Some((bundle, None)) => {
                self.desk.roles.cancel();
                self.desk.roles.skill = Some(bundle);
                self.refresh();
            }
            None => {
                self.refresh();
            }
        }
    }
    pub fn show_skill(&mut self, slug: &str) {
        let result = self.role_db().and_then(|b| b.skill_show(slug));
        if let Some(skill) = self.result(result) {
            self.desk.roles.cancel();
            self.desk.roles.skill = Some(skill);
        }
    }
    pub fn ask_delete_skill(&mut self, slug: &str) {
        let result = self.role_db().and_then(|b| b.skill_show(slug));
        if let Some(skill) = self.result(result)
            && !skill.info.is_builtin
        {
            self.desk.roles.confirm = Some(RoleConfirm::DeleteSkill(slug.into(), skill.info.roles));
        }
    }
    pub fn confirm_role_change(&mut self) {
        let Some(change) = self.desk.roles.confirm.take() else {
            return;
        };
        let result = self
            .role_db()
            .map(|db| db.role_library_as(&self.actor))
            .and_then(|writer| match change {
                RoleConfirm::DeleteSkill(slug, affected) => {
                    writer.skill_delete_confirmed(&slug, &affected)?;
                    Ok(None)
                }
                RoleConfirm::Reset => {
                    let r = self
                        .desk
                        .roles
                        .editor
                        .original
                        .as_ref()
                        .context("No role selected")?;
                    writer.role_reset(&r.slug, Some(r.version)).map(Some)
                }
                RoleConfirm::DeleteRole => {
                    let r = self
                        .desk
                        .roles
                        .editor
                        .original
                        .as_ref()
                        .context("No role selected")?;
                    writer.role_delete_if(&r.slug, Some(r.version))?;
                    Ok(None)
                }
            });
        if let Some(changed) = self.result(result) {
            if let Some(r) = changed {
                self.desk.roles.editor = Editor::from(r);
            } else if self.desk.roles.mode == RoleMode::Edit {
                self.desk.roles.set_mode(RoleMode::Roles);
            }
            self.desk.roles.skill = None;
        }
        self.refresh();
    }
}
