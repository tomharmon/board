//! Connector configuration and durable receipts. No network, secret or provider SDK lives here.
mod mapping;
mod media;
mod queue;

use crate::{Board, Message, PostOptions, errors::Kind};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub use mapping::Copy;
pub(crate) use mapping::origin_on;
pub use media::{InboundFile, SourceFile};
pub use queue::{
    Checkpoint, Delivery, DeliveryResult, Direction, Envelope, FailureReport, Job, Lease,
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
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Import {
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
    CREATE TABLE IF NOT EXISTS sync_copies(
      source TEXT NOT NULL, thread TEXT NOT NULL, external TEXT NOT NULL,
      message INTEGER NOT NULL REFERENCES messages(id), origin INTEGER NOT NULL, url TEXT,
      modified INTEGER, managed_labels TEXT NOT NULL DEFAULT '{}', PRIMARY KEY(source,thread,external), UNIQUE(source,thread,message),
      FOREIGN KEY(source,thread) REFERENCES sync_threads(source,thread));
    CREATE INDEX IF NOT EXISTS sync_copies_local ON sync_copies(message);
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
    CREATE TABLE IF NOT EXISTS sync_checkpoints(
      source TEXT NOT NULL REFERENCES sync_sources(name), stream TEXT NOT NULL, value TEXT NOT NULL,
      PRIMARY KEY(source,stream));
    CREATE TABLE IF NOT EXISTS sync_starts(
      source TEXT NOT NULL REFERENCES sync_sources(name),root INTEGER NOT NULL REFERENCES messages(id),
      item INTEGER NOT NULL REFERENCES sync_queue(id),PRIMARY KEY(source,root));
    CREATE TRIGGER IF NOT EXISTS sync_outgoing AFTER UPDATE OF message ON events
      WHEN NEW.message IS NOT NULL AND NEW.kind IN ('message.posted','message.edited','message.deleted')
       AND substr(NEW.actor,1,7) != 'source:'
      BEGIN
        INSERT OR IGNORE INTO sync_queue(source,direction,thread,event_key,payload,raw)
        SELECT s.name,'out',t.thread,CAST(NEW.cursor AS TEXT),
          json_object('event',NEW.kind,'message',NEW.message_id,'cursor',NEW.cursor,
                      'text',json_extract(NEW.message,'$.text'),'parent',json_extract(NEW.message,'$.reply_to')),'null'
          FROM sync_threads t JOIN sync_sources s ON s.name=t.source
          WHERE s.outbound=1 AND t.root=coalesce(json_extract(NEW.message,'$.re'),NEW.message_id);
      END;
    CREATE TRIGGER IF NOT EXISTS sync_outgoing_reactions AFTER UPDATE OF message ON events
      WHEN NEW.message IS NOT NULL AND NEW.kind IN ('reaction.added','reaction.removed')
        AND substr(NEW.actor,1,7)!='source:'
        AND (SELECT count(*) FROM reactions WHERE message_id=NEW.message_id AND emoji=json_extract(NEW.payload,'$.emoji')
             AND substr(actor,1,7)!='source:')=CASE NEW.kind WHEN 'reaction.added' THEN 1 ELSE 0 END
      BEGIN
        INSERT OR IGNORE INTO sync_queue(source,direction,thread,event_key,payload,raw)
        SELECT s.name,'out',t.thread,CAST(NEW.cursor AS TEXT),
          json_object('event',NEW.kind,'message',NEW.message_id,'cursor',NEW.cursor,'text','',
                      'emoji',json_extract(NEW.payload,'$.emoji'),'parent',null),'null'
        FROM sync_threads t JOIN sync_sources s ON s.name=t.source
        WHERE s.outbound=1 AND t.root=coalesce(json_extract(NEW.message,'$.re'),NEW.message_id)
          AND (s.provider='slack' OR (s.provider='linear' AND NEW.kind='reaction.added' AND t.root!=NEW.message_id))
          AND (EXISTS(SELECT 1 FROM sync_copies c WHERE c.source=s.name AND c.thread=t.thread AND c.message=NEW.message_id)
               OR EXISTS(SELECT 1 FROM sync_queue q WHERE q.source=s.name AND q.thread=t.thread AND q.direction='out' AND q.state IN ('pending','leased','sending','uncertain')
                     AND json_extract(q.payload,'$.message')=NEW.message_id AND json_extract(q.payload,'$.event') IN ('thread.start','message.posted')));
      END;")?;
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
            !name.is_empty()
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
        self.write(|tx|{let s=self.authorized_source(tx,name,who)?;
            if s.outbound!=enabled {
                tx.execute("UPDATE sync_sources SET outbound=? WHERE name=?",params![enabled,name])?;
                if !enabled { tx.execute("UPDATE sync_queue SET state='skipped',receipt=NULL,deadline=NULL,error='publishing disabled' WHERE source=? AND direction='out' AND state IN ('pending','leased')",[name])?; }
                Self::event_tx(tx,"source.configured",who,None,json!({"source":name,"channel":s.channel,"outbound":enabled}))?;
            }
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
