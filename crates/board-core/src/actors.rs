use crate::{
    Board,
    access::Action,
    errors::Kind::{Invalid, Missing},
    validate_actor, validate_channel,
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub(crate) fn init(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS actor_profiles(id TEXT PRIMARY KEY,handle TEXT NOT NULL,name TEXT NOT NULL,verified INTEGER NOT NULL DEFAULT 0);
      CREATE TABLE IF NOT EXISTS exact_mentions(message_id INTEGER NOT NULL REFERENCES messages(id),actor TEXT NOT NULL,PRIMARY KEY(message_id,actor));
      CREATE INDEX IF NOT EXISTS exact_mention_actor ON exact_mentions(actor,message_id);
      CREATE TABLE IF NOT EXISTS composer_drafts(actor TEXT NOT NULL,scope TEXT NOT NULL,body TEXT NOT NULL,channel TEXT NOT NULL,reply_to INTEGER,PRIMARY KEY(actor,scope));")?;
    // V4 indexed raw @ signs (including code); rebuild in bounded batches.
    let mut after = 0;
    loop {
        let rows = c
            .prepare("SELECT id,text FROM messages WHERE id>? ORDER BY id LIMIT 128")?
            .query_map([after], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if rows.is_empty() {
            break;
        }
        for (id, text) in rows {
            crate::core::update_mentions(c, id, &text)?;
            after = id;
        }
    }
    Ok(())
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActorEntry {
    pub id: String,
    pub handle: String,
    pub name: String,
    pub kind: String,
    pub via: Option<String>,
    pub avatar: Option<String>,
    pub verified: bool,
    pub local: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Draft {
    pub body: String,
    pub channel: String,
    pub reply_to: Option<i64>,
}

impl Board {
    pub fn actor_directory(&self, query: Option<&str>) -> Result<Vec<ActorEntry>> {
        let tx = self.conn.unchecked_transaction()?;
        let a = self.access_on(&tx)?;
        let channels = a
            .channels(&tx, Action::Read)?
            .map(|v| serde_json::to_string(&v))
            .transpose()?;
        let sql="WITH known(id) AS (SELECT actor FROM events UNION SELECT id FROM actor_profiles UNION SELECT actor FROM sync_profiles UNION SELECT ?2 WHERE ?2 IS NOT NULL),
           readable(id,actor) AS (SELECT id,actor FROM messages WHERE channel IN (SELECT value FROM json_each(?1))),
           visible(id) AS (SELECT actor FROM readable UNION SELECT r.actor FROM reactions r JOIN readable m ON m.id=r.message_id
           UNION SELECT e.actor FROM exact_mentions e JOIN readable m ON e.message_id=m.id UNION SELECT ?2 WHERE ?2 IS NOT NULL)
          SELECT k.id,p.handle,p.name,coalesce(p.verified,0),coalesce(sp.source,(SELECT cp.source FROM sync_copies cp JOIN messages seen ON seen.id=cp.message WHERE cp.origin AND seen.actor=k.id LIMIT 1)),sp.name,sp.avatar
           FROM known k LEFT JOIN actor_profiles p ON p.id=k.id LEFT JOIN sync_profiles sp ON sp.actor=k.id
           WHERE ?1 IS NULL OR k.id IN (SELECT id FROM visible)";
        let rows = tx
            .prepare(sql)?
            .query_map(params![channels, a.bound_actor()], |r| {
                let id: String = r.get(0)?;
                let p_name: Option<String> = r.get(2)?;
                let source: Option<String> = r.get(4)?;
                let external = source.is_some();
                let profile = id.split_once('/');
                let kind = if external {
                    "external"
                } else if profile.is_some() {
                    "agent"
                } else {
                    "person"
                };
                let via = source
                    .clone()
                    .or_else(|| profile.map(|(person, _)| person.to_owned()));
                let name: Option<String> = r.get(5)?;
                let handle: String = r.get::<_, Option<String>>(1)?.unwrap_or_else(|| {
                    if external {
                        format!(
                            "{}/{}",
                            source.as_deref().unwrap_or_default(),
                            profile.map(|p| p.1).unwrap_or(&id)
                        )
                    } else {
                        id.clone()
                    }
                });
                Ok(ActorEntry {
                    name: name.or(p_name).unwrap_or_else(|| id.clone()),
                    kind: kind.into(),
                    via,
                    avatar: r.get(6)?,
                    verified: !external && r.get(3)?,
                    local: !external,
                    handle,
                    id,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let q = query.unwrap_or("").to_lowercase();
        let mut rows: Vec<_> = rows
            .into_iter()
            .filter(|a| {
                [&a.id, &a.handle, &a.name]
                    .iter()
                    .any(|s| s.to_lowercase().contains(&q))
            })
            .collect();
        rows.sort_by(|a, b| {
            a.handle
                .to_lowercase()
                .cmp(&b.handle.to_lowercase())
                .then(a.id.cmp(&b.id))
        });
        tx.commit()?;
        Ok(rows)
    }
    /// Change only one's own local presentation. Neither field is an identity credential.
    pub fn set_actor_profile(&self, actor: &str, handle: &str, name: &str) -> Result<()> {
        validate_actor(actor)?;
        ensure!(
            !name.trim().is_empty() && name.len() <= 256 && !name.chars().any(char::is_control),
            "invalid display name"
        );
        ensure!(
            !handle.is_empty()
                && handle.len() <= 128
                && handle.split('/').count() <= 2
                && handle.split('/').all(|s| s
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                    && s.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric)
                    && s.bytes().all(|b| b.is_ascii_lowercase()
                        || b.is_ascii_digit()
                        || matches!(b, b'_' | b'-' | b'.'))),
            "invalid profile handle"
        );
        self.write(|tx|{let a=self.access_on(tx)?;a.actor(actor)?;
            ensure!(!actor.starts_with("source:"),Invalid.error("external_profile","imported identity is managed by its source"));
            if tx.execute("INSERT INTO actor_profiles VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE SET handle=excluded.handle,name=excluded.name,verified=excluded.verified
               WHERE handle<>excluded.handle OR name<>excluded.name OR verified<>excluded.verified",params![actor,handle,name,a.credential_id().is_some()])?!=0{
                Self::event_tx(tx,"actor.updated",actor,None,serde_json::json!({"id":actor}))?;
            }Ok(())})
    }
    fn draft_scope(&self, c: &Connection, actor: &str, scope: &str) -> Result<Option<i64>> {
        validate_actor(actor)?;
        let a = self.access_on(c)?;
        a.actor(actor)?;
        if scope == "new" {
            return Ok(None);
        }
        let id = scope
            .strip_prefix("thread:")
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|v| *v > 0)
            .context(Invalid.error("invalid_draft", "expected new or thread:ID"))?;
        a.message(c, id, Action::Read)?;
        let root: i64 = c.query_row(
            "SELECT coalesce(re,id) FROM messages WHERE id=?",
            [id],
            |r| r.get(0),
        )?;
        ensure!(
            root == id,
            Invalid.error("invalid_draft", "expected thread root")
        );
        Ok(Some(root))
    }
    pub fn load_draft(&self, actor: &str, scope: &str) -> Result<Option<Draft>> {
        let tx = self.conn.unchecked_transaction()?;
        self.draft_scope(&tx, actor, scope)?;
        let key = self.access_on(&tx)?.private_key(actor)?;
        let d = tx
            .query_row(
                "SELECT body,channel,reply_to FROM composer_drafts WHERE actor=? AND scope=?",
                [key.as_str(), scope],
                |r| {
                    Ok(Draft {
                        body: r.get(0)?,
                        channel: r.get(1)?,
                        reply_to: r.get(2)?,
                    })
                },
            )
            .optional()?;
        if let Some(d) = &d
            && !d.channel.is_empty()
        {
            let a = self.access_on(&tx)?;
            ensure!(
                a.allows(Action::Read, &d.channel) || a.allows(Action::Post, &d.channel),
                Missing.error("draft_not_found", "draft not found")
            );
        }
        tx.commit()?;
        Ok(d)
    }
    pub fn save_draft(&self, actor: &str, scope: &str, draft: Option<&Draft>) -> Result<()> {
        self.write(|tx|{let root=self.draft_scope(tx,actor,scope)?;let a=self.access_on(tx)?;let key=a.private_key(actor)?;
            let old:Option<String>=tx.query_row("SELECT channel FROM composer_drafts WHERE actor=? AND scope=?",[key.as_str(),scope],|r|r.get(0)).optional()?;
            ensure!(old.is_none_or(|s|s.is_empty() || a.allows(Action::Read,&s) || a.allows(Action::Post,&s)),Missing.error("draft_not_found","draft not found"));
            if let Some(d)=draft.filter(|d|!d.body.is_empty()){
                ensure!(d.body.chars().count()<=10_000 && !d.body.contains('\0'),"draft exceeds message limits");
                if !d.channel.is_empty(){validate_channel(&d.channel)?;a.require(Action::Post,&d.channel)?;}
                if let Some(target)=d.reply_to{let channel=a.message(tx,target,Action::Read)?;
                    let parent:i64=tx.query_row("SELECT coalesce(re,id) FROM messages WHERE id=?",[target],|r|r.get(0))?;
                    ensure!(root==Some(parent) && (d.channel.is_empty() || d.channel==channel),"draft reply target is outside its thread");}
                tx.execute("INSERT INTO composer_drafts VALUES(?,?,?,?,?) ON CONFLICT(actor,scope) DO UPDATE SET body=excluded.body,channel=excluded.channel,reply_to=excluded.reply_to
                     WHERE body<>excluded.body OR channel<>excluded.channel OR reply_to IS NOT excluded.reply_to",params![key,scope,d.body,d.channel,d.reply_to])?;
            }else{tx.execute("DELETE FROM composer_drafts WHERE actor=? AND scope=?",[key.as_str(),scope])?;}
            Ok(())})
    }
}

pub(crate) fn mention_terms(
    c: &Connection,
    a: &crate::access::Access,
    query: &str,
) -> Result<Vec<String>> {
    let term = crate::prose::query(query);
    let mut terms = vec![term.clone()];
    if a.everywhere(Action::Read) {
        let rows=c.prepare("SELECT id,handle FROM actor_profiles UNION ALL SELECT actor,source||'/'||substr(actor,instr(actor,'/')+1) FROM sync_profiles")?
            .query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for (id, handle) in rows {
            let id = crate::prose::term(&id);
            let handle = crate::prose::term(&handle);
            if id == term || handle == term {
                terms.push(id);
                terms.push(handle);
            }
        }
    }
    terms.sort();
    terms.dedup();
    Ok(terms)
}
