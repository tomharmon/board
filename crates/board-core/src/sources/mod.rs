//! Connector configuration and durable receipts. No network, secret or provider SDK lives here.
mod lookup;
mod mapping;
mod media;
mod queue;
mod render;
mod targets;
pub use targets::{ThreadLink, validate_slack_timestamp};

use crate::{Board, Message, PostOptions, errors::Kind};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub use lookup::{LocatedLink, SyncTarget, SyncTargets};
pub use mapping::Copy;
pub(crate) use mapping::origin_on;
pub(crate) use media::file_notices_on;
pub use media::{FileNotice, InboundFile, MediaRequest, SourceFile};
pub use queue::DrainReport;
pub use queue::{
    Checkpoint, Delivery, DeliveryNotice, DeliveryResult, Direction, Envelope, FailureReport, Job,
    Lease,
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Local,
    Github,
    Slack,
    Linear,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    pub provider: Provider,
    /// Repository, channel ID, or team key; never an API URL or credential.
    pub remote: String,
    pub channel: String,
    pub owner: String,
    pub outbound: bool,
    #[serde(default)]
    pub settings: SourceSettings,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SourceSettings {
    /// Automatically link new roots in the mapped channel. Sending still requires publishing and a connector.
    pub mirror_roots: bool,
    pub mirror_files: bool,
    pub display_authors: bool,
    pub github_items: GithubItems,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GithubItems {
    #[default]
    All,
    Issues,
    Pulls,
}
impl GithubItems {
    pub fn includes(self, is_pull: bool) -> bool {
        self == Self::All || (is_pull && self == Self::Pulls) || (!is_pull && self == Self::Issues)
    }
}
impl std::str::FromStr for GithubItems {
    type Err = &'static str;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "all" => Ok(Self::All),
            "issues" => Ok(Self::Issues),
            "pulls" => Ok(Self::Pulls),
            _ => Err("expected all, issues or pulls"),
        }
    }
}
/// Omitted fields retain their values, including under concurrent updates.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SourcePatch {
    pub outbound: Option<bool>,
    pub mirror_roots: Option<bool>,
    pub mirror_files: Option<bool>,
    pub display_authors: Option<bool>,
    pub github_items: Option<GithubItems>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Import {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<crate::links::Link>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_sync: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mentioned: BTreeMap<String, AuthorInfo>,
    pub thread: String,
    pub external: String,
    pub author: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<AuthorInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
    pub change: Change,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorInfo {
    pub name: String,
    #[serde(default)]
    pub avatar: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    /// A recognized provider event that has no Board effect; retain its deduplication receipt.
    Ignore,
    /// Apply labels and optionally a status reply only while the linked root still matches.
    Signal {
        conditions: BTreeMap<String, String>,
        #[serde(default)]
        set: BTreeMap<String, String>,
        #[serde(default)]
        unset: Vec<String>,
        reply: Option<String>,
    },
    Post {
        text: String,
        #[serde(default)]
        labels: BTreeMap<String, String>,
    },
    Upsert {
        text: String,
        #[serde(default)]
        labels: BTreeMap<String, String>,
    },
    /// A partial provider snapshot owns only the supplied label families. A changed generation
    /// label (such as a commit SHA) invalidates results under its configured prefix atomically.
    Reconcile {
        text: String,
        labels: BTreeMap<String, String>,
        owns: Vec<String>,
        #[serde(default)]
        reset: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        existing_only: bool,
    },
    Edit {
        text: String,
    },
    Delete,
    React {
        emoji: String,
        #[serde(default)]
        remove: bool,
    },
    Labels {
        #[serde(default)]
        set: BTreeMap<String, String>,
        #[serde(default)]
        unset: Vec<String>,
    },
}

pub(crate) fn init(c: &Connection) -> Result<()> {
    c.execute_batch("
    CREATE TABLE IF NOT EXISTS sync_sources(
      name TEXT PRIMARY KEY, provider TEXT NOT NULL, remote TEXT NOT NULL, channel TEXT NOT NULL,
      owner TEXT NOT NULL, outbound INTEGER NOT NULL DEFAULT 0);
    CREATE TABLE IF NOT EXISTS sync_threads(
      source TEXT NOT NULL REFERENCES sync_sources(name), thread TEXT NOT NULL,
      root INTEGER NOT NULL REFERENCES messages(id), PRIMARY KEY(source,thread));
    CREATE INDEX IF NOT EXISTS sync_threads_root ON sync_threads(root);
    CREATE TABLE IF NOT EXISTS sync_explicit_joins(
      source TEXT NOT NULL,thread TEXT NOT NULL,remote_root TEXT,joined_at INTEGER NOT NULL,PRIMARY KEY(source,thread),
      FOREIGN KEY(source,thread) REFERENCES sync_threads(source,thread));
    CREATE TABLE IF NOT EXISTS sync_copies(
      source TEXT NOT NULL, thread TEXT NOT NULL, external TEXT NOT NULL,
      message INTEGER NOT NULL REFERENCES messages(id), origin INTEGER NOT NULL, url TEXT,
      modified INTEGER, managed_labels TEXT NOT NULL DEFAULT '{}', PRIMARY KEY(source,thread,external), UNIQUE(source,thread,message),
      FOREIGN KEY(source,thread) REFERENCES sync_threads(source,thread));
    CREATE INDEX IF NOT EXISTS sync_copies_local ON sync_copies(message);
    CREATE INDEX IF NOT EXISTS sync_copy_permalink ON sync_copies(url) WHERE url IS NOT NULL;
    CREATE INDEX IF NOT EXISTS events_by_actor ON events(actor);
    CREATE TABLE IF NOT EXISTS sync_file_copies(
      source TEXT NOT NULL REFERENCES sync_sources(name),thread TEXT NOT NULL,external TEXT NOT NULL,
      message INTEGER NOT NULL REFERENCES messages(id),asset INTEGER NOT NULL REFERENCES file_assets(id),direction TEXT NOT NULL,
      PRIMARY KEY(source,thread,message,external,direction));
    CREATE TABLE IF NOT EXISTS sync_profiles(actor TEXT PRIMARY KEY,source TEXT NOT NULL REFERENCES sync_sources(name), name TEXT NOT NULL,avatar TEXT);
    CREATE TABLE IF NOT EXISTS sync_queue(
      id INTEGER PRIMARY KEY, source TEXT NOT NULL REFERENCES sync_sources(name), direction TEXT NOT NULL,
      thread TEXT NOT NULL, event_key TEXT NOT NULL, payload TEXT NOT NULL, raw TEXT NOT NULL,
      state TEXT NOT NULL DEFAULT 'pending', attempts INTEGER NOT NULL DEFAULT 0,
      available INTEGER NOT NULL DEFAULT 0, receipt TEXT, deadline INTEGER, result INTEGER REFERENCES messages(id),
      error TEXT, UNIQUE(source,direction,thread,event_key));
    CREATE INDEX IF NOT EXISTS sync_queue_lanes ON sync_queue(source,direction,thread,id,state);
    CREATE INDEX IF NOT EXISTS sync_queue_pending ON sync_queue(direction,state,available,id);
    CREATE INDEX IF NOT EXISTS sync_queue_message ON sync_queue(source,thread,json_extract(payload,'$.message'),id) WHERE direction='out';
    CREATE TABLE IF NOT EXISTS sync_checkpoints(
      source TEXT NOT NULL REFERENCES sync_sources(name), stream TEXT NOT NULL, value TEXT NOT NULL,
      PRIMARY KEY(source,stream));
    CREATE TABLE IF NOT EXISTS sync_starts(
      source TEXT NOT NULL REFERENCES sync_sources(name),root INTEGER NOT NULL REFERENCES messages(id),
      item INTEGER NOT NULL REFERENCES sync_queue(id),PRIMARY KEY(source,root));
    CREATE TABLE IF NOT EXISTS sync_settings(source TEXT PRIMARY KEY REFERENCES sync_sources(name),value TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS sync_import_context(singleton INTEGER PRIMARY KEY CHECK(singleton=1),source TEXT NOT NULL,thread TEXT NOT NULL,external TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS sync_rendered(source TEXT NOT NULL,thread TEXT NOT NULL,message INTEGER NOT NULL,text TEXT NOT NULL,PRIMARY KEY(source,thread,message));
    CREATE TABLE IF NOT EXISTS sync_renderers(source TEXT PRIMARY KEY REFERENCES sync_sources(name),script TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS sync_responses(source TEXT NOT NULL,thread TEXT NOT NULL,external TEXT NOT NULL,item INTEGER NOT NULL REFERENCES sync_queue(id),PRIMARY KEY(source,thread,external));
    CREATE TABLE IF NOT EXISTS sync_media_fetch(source TEXT NOT NULL,thread TEXT NOT NULL,external TEXT NOT NULL,file TEXT NOT NULL,attempts INTEGER NOT NULL DEFAULT 0,error TEXT,PRIMARY KEY(source,thread,external,file));
    CREATE TABLE IF NOT EXISTS sync_file_notes(source TEXT NOT NULL,thread TEXT NOT NULL,message INTEGER NOT NULL,asset INTEGER NOT NULL,PRIMARY KEY(source,thread,message,asset));
    CREATE TABLE IF NOT EXISTS sync_file_metadata(source TEXT NOT NULL,thread TEXT NOT NULL,external TEXT NOT NULL,file TEXT NOT NULL,message INTEGER NOT NULL,name TEXT NOT NULL,size INTEGER,PRIMARY KEY(source,thread,external,file));
    CREATE INDEX IF NOT EXISTS sync_file_metadata_message ON sync_file_metadata(message);
    CREATE TABLE IF NOT EXISTS message_links(message INTEGER NOT NULL REFERENCES messages(id),ordinal INTEGER NOT NULL,url TEXT NOT NULL,target INTEGER REFERENCES messages(id),author TEXT,text TEXT,reason TEXT,PRIMARY KEY(message,url));
    ")?;
    targets::migrate_joins(c)?;
    c.execute_batch("
    DROP TRIGGER IF EXISTS sync_outgoing;
    DROP TRIGGER IF EXISTS sync_outgoing_reactions;
    CREATE TRIGGER sync_outgoing AFTER UPDATE OF message ON events
      WHEN NEW.message IS NOT NULL AND NEW.kind IN ('message.posted','message.edited','message.deleted')
       AND NOT EXISTS(SELECT 1 FROM private_messages WHERE id IN (NEW.message_id,json_extract(NEW.message,'$.re')))
      BEGIN
        INSERT OR IGNORE INTO sync_queue(source,direction,thread,event_key,payload,raw)
        SELECT s.name,'out',t.thread,CAST(NEW.cursor AS TEXT),
          json_object('event',NEW.kind,'message',NEW.message_id,'cursor',NEW.cursor,
                      'text',json_extract(NEW.message,'$.text'),'parent',json_extract(NEW.message,'$.reply_to')),'null'
          FROM sync_threads t JOIN sync_sources s ON s.name=t.source
          WHERE s.outbound=1 AND t.root=coalesce(json_extract(NEW.message,'$.re'),NEW.message_id)
          AND (NEW.kind!='message.posted' OR t.root!=NEW.message_id)
          AND NOT EXISTS(SELECT 1 FROM sync_explicit_joins j WHERE j.source=t.source AND j.thread=t.thread AND NEW.message_id<=j.joined_at)
          AND NOT EXISTS(SELECT 1 FROM sync_import_context WHERE source=s.name);
      END;
    CREATE TRIGGER IF NOT EXISTS sync_outgoing_reactions AFTER UPDATE OF message ON events
      WHEN NEW.message IS NOT NULL AND NEW.kind IN ('reaction.added','reaction.removed')
        AND NOT EXISTS(SELECT 1 FROM private_messages WHERE id IN (NEW.message_id,json_extract(NEW.message,'$.re')))
      BEGIN
        INSERT OR IGNORE INTO sync_queue(source,direction,thread,event_key,payload,raw)
        SELECT s.name,'out',t.thread,CAST(NEW.cursor AS TEXT),
          json_object('event',NEW.kind,'message',NEW.message_id,'cursor',NEW.cursor,'text','',
                      'emoji',json_extract(NEW.payload,'$.emoji'),'parent',null),'null'
        FROM sync_threads t JOIN sync_sources s ON s.name=t.source
        WHERE s.outbound=1 AND t.root=coalesce(json_extract(NEW.message,'$.re'),NEW.message_id)
          AND (s.provider='slack' OR (s.provider='linear' AND NEW.kind='reaction.added' AND t.root!=NEW.message_id))
          AND NOT EXISTS(SELECT 1 FROM sync_explicit_joins j WHERE j.source=t.source AND j.thread=t.thread AND NEW.message_id<=j.joined_at)
          AND NOT EXISTS(SELECT 1 FROM sync_import_context WHERE source=s.name)
          AND (SELECT count(*) FROM reactions WHERE message_id=NEW.message_id AND emoji=json_extract(NEW.payload,'$.emoji')
               AND instr(actor,'source:'||s.name||'/')!=1)=CASE NEW.kind WHEN 'reaction.added' THEN 1 ELSE 0 END
          AND (EXISTS(SELECT 1 FROM sync_copies c WHERE c.source=s.name AND c.thread=t.thread AND c.message=NEW.message_id)
               OR EXISTS(SELECT 1 FROM sync_queue q WHERE q.source=s.name AND q.thread=t.thread AND q.direction='out' AND q.state IN ('pending','leased','sending','uncertain')
                     AND json_extract(q.payload,'$.message')=NEW.message_id AND json_extract(q.payload,'$.event') IN ('thread.start','message.posted')));
      END;")?;
    c.execute_batch(include_str!("automirror.sql"))?;
    Ok(())
}

fn ident(s: &str) -> Result<()> {
    ensure!(
        !s.is_empty() && s.len() <= 512 && s.trim() == s && !s.chars().any(char::is_control),
        "invalid external identifier"
    );
    Ok(())
}
fn src_on(c: &Connection, name: &str) -> Result<Source> {
    let (kind, remote, channel, owner, outbound): (String, String, String, String, bool) = c
        .query_row(
            "SELECT provider,remote,channel,owner,outbound FROM sync_sources WHERE name=?",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?
        .with_context(|| Kind::Missing.error("source_not_found", "unknown source"))?;
    Ok(Source {
        name: name.into(),
        provider: serde_json::from_value(json!(kind))?,
        remote,
        channel,
        owner,
        outbound,
        settings: c
            .query_row(
                "SELECT value FROM sync_settings WHERE source=?",
                [name],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(|v| serde_json::from_str(&v))
            .transpose()?
            .unwrap_or_default(),
    })
}
fn owned(s: &Source, who: &str) -> Result<()> {
    ensure!(
        crate::core::owned_by(&s.owner, who),
        Kind::Missing.error("source_not_found", "unknown source")
    );
    Ok(())
}

impl Board {
    pub fn configure_source(
        &self,
        who: &str,
        name: &str,
        settings: &SourceSettings,
    ) -> Result<Source> {
        self.patch_source(
            who,
            name,
            &SourcePatch {
                mirror_roots: Some(settings.mirror_roots),
                mirror_files: Some(settings.mirror_files),
                display_authors: Some(settings.display_authors),
                github_items: Some(settings.github_items),
                ..Default::default()
            },
        )
    }
    fn authorized_source(&self, c: &Connection, name: &str, who: &str) -> Result<Source> {
        let s = src_on(c, name)?;
        owned(&s, who)?;
        self.source_access(c, who, &s.channel)?;
        Ok(s)
    }
    fn source_access(&self, c: &Connection, who: &str, channel: &str) -> Result<()> {
        let access = self.access_on(c)?;
        access.actor(who)?;
        ensure!(
            !access.integration_narrowed(),
            Kind::Forbidden.error(
                "narrowed",
                "restricted credentials cannot administer integrations"
            )
        );
        access.require(crate::access::Action::Read, channel)?;
        access.require(crate::access::Action::Sync, channel)
    }
    pub fn create_source(
        &self,
        who: &str,
        name: &str,
        provider: Provider,
        remote: &str,
        channel: &str,
    ) -> Result<(Source, bool)> {
        ensure!(
            name != "board"
                && !name.is_empty()
                && name.len() <= 48
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "source name must be lowercase letters, numbers or hyphens"
        );
        ident(remote)?;
        crate::validate_channel(channel)?;
        crate::validate_actor(who)?;
        self.write(|tx| {
            self.source_access(tx,who,channel)?;
            let kind=serde_json::to_value(&provider)?;
            let fresh=tx.execute("INSERT OR IGNORE INTO sync_sources(name,provider,remote,channel,owner) VALUES(?,?,?,?,?)",params![name,kind.as_str(),remote,channel,who])?!=0;
            let s=self.authorized_source(tx,name,who)?;
            ensure!(s.provider==provider && s.remote==remote && s.channel==channel,Kind::Conflict.error("source_exists","source already has another mapping"));
            if fresh { Self::event_tx(tx,"source.created",who,None,json!({"source":name,"channel":channel}))?; }
            Ok((s,fresh))
        })
    }
    pub fn source(&self, who: &str, name: &str) -> Result<Source> {
        let tx = self.conn.unchecked_transaction()?;
        let s = self.authorized_source(&tx, name, who)?;
        tx.commit()?;
        Ok(s)
    }
    pub fn sources(&self, who: &str) -> Result<Vec<Source>> {
        let tx = self.conn.unchecked_transaction()?;
        let access = self.access_on(&tx)?;
        access.actor(who)?;
        ensure!(
            !access.integration_narrowed(),
            Kind::Forbidden.error(
                "narrowed",
                "restricted credentials cannot list integrations"
            )
        );
        let names = tx
            .prepare("SELECT name FROM sync_sources ORDER BY name")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        let mut out = Vec::new();
        for name in names {
            let s = src_on(&tx, &name)?;
            if crate::core::owned_by(&s.owner, who)
                && access.allows(crate::access::Action::Read, &s.channel)
                && access.allows(crate::access::Action::Sync, &s.channel)
            {
                out.push(s);
            }
        }
        tx.commit()?;
        Ok(out)
    }
    pub fn set_outbound(&self, who: &str, name: &str, enabled: bool) -> Result<Source> {
        self.patch_source(
            who,
            name,
            &SourcePatch {
                outbound: Some(enabled),
                ..Default::default()
            },
        )
    }
    pub fn patch_source(&self, who: &str, name: &str, patch: &SourcePatch) -> Result<Source> {
        self.write(|tx|{let s=self.authorized_source(tx,name,who)?;
            let enabled=patch.outbound.unwrap_or(s.outbound);
            let settings=SourceSettings {mirror_roots:patch.mirror_roots.unwrap_or(s.settings.mirror_roots),
                mirror_files:patch.mirror_files.unwrap_or(s.settings.mirror_files),display_authors:patch.display_authors.unwrap_or(s.settings.display_authors),
                github_items:patch.github_items.unwrap_or(s.settings.github_items)};
            ensure!(settings.github_items==GithubItems::All||s.provider==Provider::Github,"issue/PR routing requires a GitHub source");
            ensure!(!settings.mirror_files||s.provider==Provider::Slack,"file mirroring requires a Slack source");
            if s.settings.mirror_files && !settings.mirror_files {
                tx.execute("UPDATE sync_queue SET state='skipped',error='file mirroring disabled',receipt=NULL,deadline=NULL
                    WHERE source=? AND direction='out' AND state IN ('pending','leased') AND json_extract(payload,'$.event')='file.share'",[name])?;
            }
            ensure!(!settings.mirror_roots||s.provider!=Provider::Github,"GitHub does not automatically open issues; use sync start for an intentional new issue");
            if settings!=s.settings{tx.execute("INSERT INTO sync_settings VALUES(?,?) ON CONFLICT(source) DO UPDATE SET value=excluded.value",params![name,serde_json::to_string(&settings)?])?;}
            if s.outbound!=enabled {
                tx.execute("UPDATE sync_sources SET outbound=? WHERE name=?",params![enabled,name])?;
                if !enabled { tx.execute("UPDATE sync_queue SET state='skipped',receipt=NULL,deadline=NULL,error='publishing disabled' WHERE source=? AND direction='out' AND state IN ('pending','leased')",[name])?;queue::abandon_skipped_starts(tx,name)?; }
            }
            if enabled!=s.outbound||settings!=s.settings{Self::event_tx(tx,"source.configured",who,None,json!({"source":name,"channel":s.channel,"outbound":enabled,"settings":settings}))?;}
            src_on(tx,name)})
    }
    pub fn source_checkpoint(&self, who: &str, name: &str, stream: &str) -> Result<Option<String>> {
        let tx = self.conn.unchecked_transaction()?;
        self.authorized_source(&tx, name, who)?;
        let v = tx
            .query_row(
                "SELECT value FROM sync_checkpoints WHERE source=? AND stream=?",
                params![name, stream],
                |r| r.get(0),
            )
            .optional()?;
        tx.commit()?;
        Ok(v)
    }
}
