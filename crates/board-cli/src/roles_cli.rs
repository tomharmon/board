use anyhow::{Result, ensure};
use board_local::{Board, roles::Role};
use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::json;
use std::path::PathBuf;

#[derive(Args)]
pub struct RoleFields {
    #[arg(long)]
    display_name: Option<String>,
    #[arg(long)]
    description: Option<String>,
    #[arg(long, conflicts_with = "prompt_file")]
    prompt: Option<String>,
    #[arg(long)]
    prompt_file: Option<PathBuf>,
}
impl RoleFields {
    fn prompt(&self) -> Result<Option<String>> {
        self.prompt_file
            .as_deref()
            .map(crate::load_text)
            .transpose()
            .map(|p| p.or_else(|| self.prompt.clone()))
    }
}
#[derive(Subcommand)]
pub enum Roles {
    List,
    Show {
        slug: String,
    },
    Create {
        slug: String,
        #[command(flatten)]
        fields: RoleFields,
    },
    Edit {
        slug: String,
        #[arg(long)]
        new_slug: Option<String>,
        #[arg(long)]
        if_version: Option<i64>,
        #[command(flatten)]
        fields: RoleFields,
    },
    /// Restore the built-in name, description and prompt; keep attached skills.
    Reset {
        slug: String,
        #[arg(long)]
        if_version: Option<i64>,
    },
    Delete {
        slug: String,
        #[arg(long)]
        if_version: Option<i64>,
    },
    AttachSkill {
        role: String,
        skill: String,
    },
    DetachSkill {
        role: String,
        skill: String,
    },
}
#[derive(Subcommand)]
pub enum Skills {
    List,
    Show {
        slug: String,
    },
    /// Import a local SKILL.md folder; importing the same slug updates the library version.
    Import {
        directory: PathBuf,
    },
    Delete {
        slug: String,
        #[arg(long)]
        force: bool,
    },
}
pub(crate) fn output<T: Serialize>(
    js: bool,
    value: &T,
    human: impl FnOnce() -> String,
) -> Result<()> {
    if js {
        println!("{}", serde_json::to_string(value)?)
    } else {
        println!("{}", crate::safe(&human()))
    }
    Ok(())
}
fn describe(r: &Role) -> String {
    format!(
        "{} ({})  v{}\n{}\n\n{}\n\nSkills: {}",
        r.name,
        r.slug,
        r.version,
        r.description,
        r.system_prompt,
        r.skills.join(", ")
    )
}
pub fn roles(b: &Board, actor: &str, cmd: Roles, js: bool) -> Result<()> {
    let writer = b.role_library_as(actor);
    let r = match cmd {
        Roles::List => {
            let list = b.roles_list()?;
            return output(js, &list, || {
                list.iter()
                    .map(|r| format!("{}  v{}  {}", r.slug, r.version, r.description))
                    .collect::<Vec<_>>()
                    .join("\n")
            });
        }
        Roles::Show { slug } => b.role_show(&slug)?,
        Roles::Create { slug, fields } => writer.role_create(
            &slug,
            fields.display_name.as_deref().unwrap_or(&slug),
            fields.description.as_deref().unwrap_or_default(),
            fields.prompt()?.as_deref().unwrap_or_default(),
        )?,
        Roles::Edit {
            slug,
            new_slug,
            if_version,
            fields,
        } => {
            let old = b.role_show(&slug)?;
            let p = fields.prompt()?;
            ensure!(
                new_slug.is_some()
                    || fields.display_name.is_some()
                    || fields.description.is_some()
                    || p.is_some(),
                "provide at least one field to edit"
            );
            writer.role_edit(
                &slug,
                new_slug.as_deref().unwrap_or(&slug),
                fields.display_name.as_deref().unwrap_or(&old.name),
                fields.description.as_deref().unwrap_or(&old.description),
                p.as_deref().unwrap_or(&old.system_prompt),
                if_version.or(Some(old.version)),
            )?
        }
        Roles::Reset { slug, if_version } => writer.role_reset(&slug, if_version)?,
        Roles::Delete { slug, if_version } => {
            writer.role_delete_if(&slug, if_version)?;
            return output(js, &json!({"deleted":slug}), || format!("Deleted {slug}"));
        }
        Roles::AttachSkill { role, skill } => writer.role_attach_skill(&role, &skill)?,
        Roles::DetachSkill { role, skill } => writer.role_detach_skill(&role, &skill)?,
    };
    output(js, &r, || describe(&r))
}
pub fn skills(b: &Board, actor: &str, cmd: Skills, js: bool) -> Result<()> {
    let writer = b.role_library_as(actor);
    match cmd {
        Skills::List => {
            let v = b.skills_list()?;
            output(js, &v, || {
                v.iter()
                    .map(|s| format!("{}  v{}  {}", s.slug, s.version, s.description))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
        Skills::Show { slug } => {
            let v = b.skill_show(&slug)?;
            output(js, &v, || {
                format!(
                    "{}  v{}\n{}\nAttached to: {}\n\n{}",
                    v.info.slug,
                    v.info.version,
                    v.info.description,
                    v.info.roles.join(", "),
                    v.files.get("SKILL.md").map_or("", String::as_str)
                )
            })
        }
        Skills::Import { directory } => {
            let v = writer.skill_import(&directory)?;
            output(js, &v, || {
                format!(
                    "Imported {} v{} ({} files)",
                    v.info.slug,
                    v.info.version,
                    v.files.len()
                )
            })
        }
        Skills::Delete { slug, force } => {
            let affected = writer.delete_skill_bundle(&slug, force)?;
            output(
                js,
                &json!({"deleted":slug,"affected_roles":affected}),
                || format!("Deleted {slug}"),
            )
        }
    }
}
