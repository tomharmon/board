use crate::{
    Board, Draft, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS, access::Action, errors::Kind,
    validate_channel,
};
use anyhow::Context;
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// Draft values survive restart; posting is always an explicit, immediate action.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ComposerSettings {
    pub no_sync: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskDraftSettings>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TaskDraftSettings {
    pub needs: String,
    pub assignee: String,
}
impl TaskDraftSettings {
    pub fn parsed_needs(&self) -> Result<Vec<i64>> {
        self.needs
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|s| !s.is_empty())
            .map(|s| {
                s.trim_start_matches('#')
                    .parse::<i64>()
                    .ok()
                    .filter(|id| *id > 0)
                    .context("Prerequisites must be positive task IDs")
            })
            .collect()
    }
}
pub struct SavedComposer {
    pub draft: Draft,
    pub settings: ComposerSettings,
    pub files: Vec<(String, Vec<u8>)>,
    pub stamp: String,
}
pub(crate) fn root_scope(scope: &str, channel: &str) -> bool {
    scope == "new" || scope.strip_prefix("channel:") == Some(channel)
}
pub(crate) fn init(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS composer_settings(actor TEXT NOT NULL,scope TEXT NOT NULL,
        stamp TEXT NOT NULL,settings TEXT NOT NULL,PRIMARY KEY(actor,scope),
        FOREIGN KEY(actor,scope) REFERENCES composer_drafts(actor,scope) ON DELETE CASCADE);
        CREATE TABLE IF NOT EXISTS composer_files(actor TEXT NOT NULL,scope TEXT NOT NULL,
        ordinal INTEGER NOT NULL,name TEXT NOT NULL,data BLOB NOT NULL,PRIMARY KEY(actor,scope,ordinal),
        FOREIGN KEY(actor,scope) REFERENCES composer_drafts(actor,scope) ON DELETE CASCADE);
        CREATE TRIGGER IF NOT EXISTS composer_stamp_on_insert AFTER INSERT ON composer_drafts BEGIN
            INSERT INTO composer_settings VALUES(new.actor,new.scope,lower(hex(randomblob(16))),'{}'); END;
        CREATE TRIGGER IF NOT EXISTS composer_stamp_on_update AFTER UPDATE OF body,channel,reply_to ON composer_drafts BEGIN
            UPDATE composer_settings SET stamp=lower(hex(randomblob(16))) WHERE actor=new.actor AND scope=new.scope; END;")?;
    Ok(())
}

fn conflict() -> crate::errors::Rejection {
    Kind::Conflict.error(
        "draft_changed",
        "Draft changed in another window; reload the saved draft before sending",
    )
}
impl Board {
    fn composer_key(&self, c: &Connection, actor: &str, scope: &str) -> Result<String> {
        self.draft_scope(c, actor, scope)?;
        let access = self.access_on(c)?;
        let key = access.private_key(actor)?;
        let old: Option<String> = c
            .query_row(
                "SELECT channel FROM composer_drafts WHERE actor=? AND scope=?",
                [&key, scope],
                |r| r.get(0),
            )
            .optional()?;
        ensure!(
            old.is_none_or(|s| s.is_empty()
                || access.allows(Action::Read, &s)
                || access.allows(Action::Post, &s)),
            Kind::Missing.error("draft_not_found", "draft not found")
        );
        Ok(key)
    }
    pub fn load_composer(&self, actor: &str, scope: &str) -> Result<Option<SavedComposer>> {
        let tx = self.conn.unchecked_transaction()?;
        let key = self.composer_key(&tx, actor, scope)?;
        let row = tx
            .query_row(
                "SELECT d.body,d.channel,d.reply_to,s.settings,s.stamp FROM composer_drafts d
            JOIN composer_settings s USING(actor,scope) WHERE d.actor=? AND d.scope=?",
                [&key, scope],
                |r| {
                    Ok((
                        Draft {
                            body: r.get(0)?,
                            channel: r.get(1)?,
                            reply_to: r.get(2)?,
                        },
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;
        let saved = if let Some((draft, settings, stamp)) = row {
            let files=tx.prepare("SELECT name,data FROM composer_files WHERE actor=? AND scope=? ORDER BY ordinal")?
                .query_map([&key,scope],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
            Some(SavedComposer {
                draft,
                settings: serde_json::from_str(&settings)?,
                files,
                stamp,
            })
        } else {
            None
        };
        tx.commit()?;
        Ok(saved)
    }
    pub(crate) fn composer_settings_on(
        &self,
        c: &Connection,
        actor: &str,
        scope: &str,
    ) -> Result<ComposerSettings> {
        let key = self.composer_key(c, actor, scope)?;
        let saved: String = c
            .query_row(
                "SELECT settings FROM composer_settings WHERE actor=? AND scope=?",
                [&key, scope],
                |r| r.get(0),
            )
            .optional()?
            .context(conflict())?;
        Ok(serde_json::from_str(&saved)?)
    }
    /// Only supply files after the attachment queue changes; editing one character does not rewrite BLOBs.
    pub fn save_composer(
        &self,
        actor: &str,
        scope: &str,
        old_stamp: Option<&str>,
        draft: Option<(&Draft, &ComposerSettings)>,
        files: Option<&[(String, Vec<u8>)]>,
    ) -> Result<Option<String>> {
        self.write(|tx| {
            let key = self.composer_key(tx, actor, scope)?;
            let stamp: Option<String> = tx.query_row(
                "SELECT stamp FROM composer_settings WHERE actor=? AND scope=?",
                [&key, scope], |r| r.get(0),
            ).optional()?;
            ensure!(stamp.as_deref() == old_stamp, conflict());
            let Some((d, settings)) = draft else {
                tx.execute("DELETE FROM composer_drafts WHERE actor=? AND scope=?", [&key, scope])?;
                return Ok(None);
            };
            ensure!(d.body.encode_utf16().count() <= 10_000 && !d.body.contains('\0'), "draft exceeds message limits");
            let access = self.access_on(tx)?;
            ensure!(scope.strip_prefix("channel:").is_none_or(|channel| channel == d.channel),
                Kind::Invalid.error("invalid_draft", "draft destination differs from its channel"));
            if !d.channel.is_empty() {
                validate_channel(&d.channel)?;
                access.require(Action::Post, &d.channel)?;
            }
            ensure!(settings.task.is_some() == (scope == "tasks"), "Task routing belongs to a task draft");
            ensure!(settings.task.is_none() || settings.after.is_none(), "Task drafts cannot be reply-guarded");
            for id in [d.reply_to.or(self.draft_scope(tx, actor, scope)?), settings.after].into_iter().flatten() {
                let channel = access.message(tx, id, Action::Read)?;
                let root: i64 = tx.query_row(
                    "SELECT coalesce(re,id) FROM messages WHERE id=?", [id], |r| r.get(0),
                )?;
                ensure!(scope == format!("thread:{root}") && (d.channel.is_empty() || channel == d.channel),
                    "draft reply target is outside its thread");
            }
            let settings = serde_json::to_string(settings)?;
            ensure!(settings.len() <= 64 * 1024, "delivery options exceed draft limit");
            tx.execute(
                "INSERT INTO composer_drafts VALUES(?,?,?,?,?) ON CONFLICT(actor,scope) DO UPDATE SET
                 body=excluded.body,channel=excluded.channel,reply_to=excluded.reply_to",
                params![key, scope, d.body, d.channel, d.reply_to],
            )?;
            if let Some(files) = files {
                ensure!(files.len() <= MAX_ATTACHMENTS, "too many attachments");
                tx.execute("DELETE FROM composer_files WHERE actor=? AND scope=?", [&key, scope])?;
                for (index, (name, data)) in files.iter().enumerate() {
                    crate::files::valid_name(name)?;
                    ensure!(data.len() <= MAX_ATTACHMENT_BYTES, "attachment exceeds 4 MiB");
                    tx.execute(
                        "INSERT INTO composer_files VALUES(?,?,?,?,?)",
                        params![key, scope, index as i64, name, data],
                    )?;
                }
            }
            let stamp: String = tx.query_row(
                "UPDATE composer_settings SET stamp=lower(hex(randomblob(16))),settings=?
                 WHERE actor=? AND scope=? RETURNING stamp",
                params![settings, key, scope], |r| r.get(0),
            )?;
            Ok(Some(stamp))
        })
    }
    pub(crate) fn check_composer_options_on(
        &self,
        c: &Connection,
        actor: &str,
        scope: &str,
        text: &str,
        opts: &crate::PostOptions,
    ) -> Result<Draft> {
        let key = self.composer_key(c, actor, scope)?;
        let (draft, settings) = c
            .query_row(
                "SELECT d.body,d.channel,d.reply_to,s.settings FROM composer_drafts d
            JOIN composer_settings s USING(actor,scope) WHERE d.actor=? AND d.scope=?",
                [&key, scope],
                |r| {
                    Ok((
                        Draft {
                            body: r.get(0)?,
                            channel: r.get(1)?,
                            reply_to: r.get(2)?,
                        },
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?
            .context(conflict())?;
        let settings: ComposerSettings = serde_json::from_str(&settings)?;
        ensure!(
            draft.body == text
                && settings.no_sync == opts.no_sync
                && opts.key.is_none()
                && settings.after == opts.after
                && (opts.after.is_none() || scope.starts_with("thread:"))
                && opts.attachment_ids.is_empty(),
            conflict()
        );
        let mut stmt = c.prepare(
            "SELECT name,data FROM composer_files WHERE actor=? AND scope=? ORDER BY ordinal",
        )?;
        let mut rows = stmt.query([&key, scope])?;
        for (name, data) in &opts.files {
            let row = rows.next()?.context(conflict())?;
            ensure!(
                row.get_ref(0)?.as_str()? == name && row.get_ref(1)?.as_blob()? == data,
                conflict()
            );
        }
        ensure!(rows.next()?.is_none(), conflict());
        Ok(draft)
    }
    /// Called inside the same immediate transaction as the post; every failure rolls this back.
    pub(crate) fn consume_composer_on(
        &self,
        c: &Connection,
        actor: &str,
        scope: &str,
        stamp: &str,
    ) -> Result<()> {
        let key = self.composer_key(c, actor, scope)?;
        ensure!(
            c.execute(
                "DELETE FROM composer_drafts WHERE actor=?1 AND scope=?2 AND EXISTS(
            SELECT 1 FROM composer_settings WHERE actor=?1 AND scope=?2 AND stamp=?3)",
                params![key, scope, stamp]
            )? == 1,
            conflict()
        );
        Ok(())
    }
}
