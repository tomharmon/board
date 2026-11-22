//! Local credentials bind a fixed actor to a board. Grants can change independently
//! of a credential; authorization is read in the same SQLite transaction as the data.
mod policy;
mod private_channels;
mod registry;
pub use registry::{Credential, Decision, Grant, GrantBatch, GrantInput, GrantSpec, Issued};

use crate::{
    Board,
    errors::Kind::{Forbidden, Invalid, Missing},
    validate_channel,
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn init(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS access_grants(
       id INTEGER PRIMARY KEY AUTOINCREMENT,subject TEXT NOT NULL,rules TEXT NOT NULL);
       CREATE INDEX IF NOT EXISTS access_grants_by_subject ON access_grants(subject);
       CREATE TABLE IF NOT EXISTS access_members(grant_id INTEGER NOT NULL REFERENCES access_grants(id) ON DELETE CASCADE,
       subject TEXT NOT NULL,position INTEGER NOT NULL,PRIMARY KEY(grant_id,subject));
       CREATE INDEX IF NOT EXISTS access_members_by_subject ON access_members(subject,grant_id);
       INSERT OR IGNORE INTO access_members SELECT id,subject,0 FROM access_grants;
       CREATE TABLE IF NOT EXISTS access_credentials(
       id TEXT PRIMARY KEY,digest BLOB UNIQUE NOT NULL,actor TEXT NOT NULL,scope TEXT,expires INTEGER,revoked INTEGER NOT NULL DEFAULT 0);
       CREATE TABLE IF NOT EXISTS blind_posts(id INTEGER PRIMARY KEY REFERENCES messages(id));
       CREATE TABLE IF NOT EXISTS activity_seen(actor TEXT PRIMARY KEY,cursor INTEGER NOT NULL);
       CREATE TABLE IF NOT EXISTS channel_seen(actor TEXT NOT NULL,channel TEXT NOT NULL REFERENCES channels(name),
           message INTEGER NOT NULL,PRIMARY KEY(actor,channel));")?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Read,
    Post,
    React,
    Label,
    Sync,
    Manage,
}

const ALL: [Action; 6] = [
    Action::Read,
    Action::Post,
    Action::React,
    Action::Label,
    Action::Sync,
    Action::Manage,
];
type Actions = BTreeSet<Action>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Rules(pub BTreeMap<String, Actions>);
#[derive(Debug, Serialize)]
pub struct Viewer {
    pub actor: String,
    pub principal: String,
    pub profile: Option<String>,
    pub handle: String,
    pub name: String,
    pub avatar: Option<String>,
    pub can: Rules,
    pub cursor: i64,
    pub owner: bool,
}

impl<'de> Deserialize<'de> for Rules {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        use serde::de::Error;
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Verbs {
            Name(String),
            Actions(Actions),
        }
        impl Verbs {
            fn read(self) -> Result<Actions> {
                match self {
                    Self::Name(s) if s == "all" => Ok(ALL.into_iter().collect()),
                    Self::Name(s) if s == "none" => Ok(Actions::new()),
                    Self::Name(_) => anyhow::bail!("expected all, none, or an action array"),
                    Self::Actions(xs) => Ok(xs),
                }
            }
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Form {
            Any(Verbs),
            Map(BTreeMap<String, Verbs>),
        }
        let map = match Form::deserialize(deserializer)? {
            Form::Any(v) => [("*".into(), v)].into(),
            Form::Map(m) => m,
        };
        let rules = Self(
            map.into_iter()
                .map(|(k, v)| Ok((k, v.read()?)))
                .collect::<Result<_>>()
                .map_err(D::Error::custom)?,
        );
        rules.validate().map_err(D::Error::custom)?;
        Ok(rules)
    }
}

impl Rules {
    pub fn parse(raw: &str) -> Result<Self> {
        ensure!(
            !raw.is_empty() && raw.len() <= 4000,
            Invalid.error("invalid_scope", "scope must contain 1-4000 bytes")
        );
        let mut entries = BTreeMap::new();
        for entry in raw.split(';') {
            let (pattern, list) = if let Some(pair) = entry.split_once('=') {
                pair
            } else if !raw.contains([';', '=']) {
                ("*", entry)
            } else {
                anyhow::bail!(Invalid.error("invalid_scope", "expected pattern=actions"))
            };
            let actions = match list {
                "all" => ALL.into_iter().collect(),
                "none" | "" => Actions::new(),
                _ => list
                    .split(',')
                    .map(|s| {
                        serde_json::from_value(serde_json::Value::String(s.to_owned())).map_err(
                            |_| Invalid.error("invalid_scope", format!("unknown permission: {s}")),
                        )
                    })
                    .collect::<std::result::Result<Actions, _>>()?,
            };
            ensure!(
                entries.insert(pattern.to_owned(), actions).is_none(),
                Invalid.error("invalid_scope", "duplicate pattern")
            );
        }
        let rules = Self(entries);
        rules.validate()?;
        Ok(rules)
    }
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            !self.0.is_empty() && self.0.len() <= 128 && serde_json::to_vec(self)?.len() <= 16384,
            Invalid.error("invalid_scope", "invalid policy size")
        );
        for p in self.0.keys().filter(|p| p.as_str() != "*") {
            validate_channel(p.strip_suffix('/').unwrap_or(p)).context(Invalid.error(
                "invalid_scope",
                "use *, an exact channel, or a subtree ending in /",
            ))?;
        }
        Ok(())
    }
    fn permits(&self, action: Action, channel: &str) -> bool {
        let selected = self
            .0
            .get(channel)
            .or_else(|| {
                self.0
                    .iter()
                    .filter_map(|(k, v)| {
                        let root = k.strip_suffix('/')?;
                        (channel == root || channel.starts_with(k)).then_some((k.len(), v))
                    })
                    .max_by_key(|(len, _)| *len)
                    .map(|(_, a)| a)
            })
            .or_else(|| self.0.get("*"));
        selected.is_some_and(|actions| {
            actions.contains(&Action::Manage)
                || actions.contains(&action)
                || (action == Action::Read
                    && actions
                        .iter()
                        .any(|a| matches!(a, Action::React | Action::Label | Action::Sync)))
        })
    }
}

#[derive(Default, Clone)]
pub(crate) enum Session {
    #[default]
    Owner,
    Rejected,
    NarrowedOwner {
        actor: String,
        cap: Rules,
    },
    Credential {
        id: String,
        cap: Option<Rules>,
    },
}

pub(crate) struct Access {
    actor: Option<String>,
    id: Option<String>,
    policy: Option<Rules>,
    own: Option<Rules>,
    caps: Vec<Rules>,
    extra_cap: bool,
}

impl Access {
    pub(crate) fn owner() -> Self {
        Self {
            actor: None,
            id: None,
            policy: None,
            own: None,
            caps: vec![],
            extra_cap: false,
        }
    }
    pub(crate) fn is_trusted(&self) -> bool {
        self.actor.is_none()
    }
    pub(crate) fn credential_id(&self) -> Option<&str> {
        self.id.as_deref()
    }
    pub(crate) fn bound_actor(&self) -> Option<&str> {
        self.actor.as_deref()
    }
    pub(crate) fn request_narrowed(&self) -> bool {
        self.extra_cap
    }
    pub(crate) fn integration_narrowed(&self) -> bool {
        !self.caps.is_empty()
    }
    pub(crate) fn actor(&self, requested: &str) -> Result<()> {
        ensure!(
            self.actor.as_deref().is_none_or(|a| a == requested),
            Forbidden.error("actor_mismatch", "credential is bound to a different actor")
        );
        Ok(())
    }
    pub(crate) fn private_key(&self, actor: &str) -> Result<String> {
        self.actor(actor)?;
        if self.is_trusted() {
            return Ok(actor.to_owned());
        }
        let cap = Sha256::digest(serde_json::to_vec(&self.caps)?);
        Ok(format!(
            "{actor}\n{}\n{cap:x}",
            self.id.as_deref().unwrap_or("owner")
        ))
    }
    pub(crate) fn allows(&self, action: Action, channel: &str) -> bool {
        self.is_trusted()
            || ((self.id.is_none()
                || self
                    .policy
                    .as_ref()
                    .is_some_and(|g| g.permits(action, channel)))
                && self.caps.iter().all(|g| g.permits(action, channel)))
    }
    pub(crate) fn everywhere(&self, action: Action) -> bool {
        if self.is_trusted() {
            return true;
        }
        if !self.allows(action, "!") {
            return false;
        }
        // Exact names and one otherwise-unmentioned child partition all possible paths.
        self.policy
            .iter()
            .chain(&self.caps)
            .flat_map(|r| r.0.keys())
            .filter(|p| p.as_str() != "*")
            .all(|p| {
                let root = p.trim_end_matches('/');
                self.allows(action, root) && self.allows(action, &format!("{root}/!"))
            })
    }
    pub(crate) fn anywhere(&self, action: Action, scope: Option<&str>) -> bool {
        let scope = scope.unwrap_or("*");
        if scope != "*" && !scope.ends_with('/') {
            return self.allows(action, scope);
        }
        let covers =
            |s: &str| scope == "*" || scope.trim_end_matches('/') == s || s.starts_with(scope);
        (scope != "*" && self.allows(action, scope.trim_end_matches('/')))
            || self.allows(action, &Self::pattern_sample(scope))
            || self
                .policy
                .iter()
                .chain(&self.caps)
                .flat_map(|p| p.0.keys())
                .filter(|p| p.as_str() != "*")
                .any(|p| {
                    let root = p.trim_end_matches('/');
                    let child = format!("{root}/!");
                    (covers(root) && self.allows(action, root))
                        || (covers(&child) && self.allows(action, &child))
                })
    }
    fn pattern_sample(pattern: &str) -> String {
        if pattern == "*" {
            "!".into()
        } else if pattern.ends_with('/') {
            format!("{pattern}!")
        } else {
            pattern.into()
        }
    }
    pub(crate) fn manage_at(&self, pattern: &str, own: bool) -> bool {
        let channel = Self::pattern_sample(pattern);
        if !own {
            return self.allows(Action::Manage, &channel);
        }
        (self.id.is_none()
            || self
                .own
                .as_ref()
                .is_some_and(|p| p.permits(Action::Manage, &channel)))
            && self
                .caps
                .iter()
                .all(|cap| cap.permits(Action::Manage, &channel))
    }
    fn list_grants(&self) -> Result<()> {
        ensure!(
            self.manage_at("*", false),
            Forbidden.error("missing_permission", "listing grants requires manage at *")
        );
        Ok(())
    }
    fn any_manage(&self) -> Result<()> {
        ensure!(
            self.is_trusted()
                || self.manage_at("*", false)
                || self
                    .policy
                    .iter()
                    .chain(&self.caps)
                    .flat_map(|p| p.0.keys())
                    .any(|s| self.manage_at(s, false)),
            Forbidden.error("missing_permission", "manage is required to change grants")
        );
        Ok(())
    }
    pub(crate) fn require(&self, action: Action, channel: &str) -> Result<()> {
        ensure!(
            self.allows(action, channel),
            Forbidden.error(
                "missing_permission",
                format!("{action:?} is not granted in {channel}")
            )
        );
        Ok(())
    }
    pub(crate) fn trusted(&self) -> Result<()> {
        ensure!(
            self.is_trusted(),
            Forbidden.error("owner_required", "operation requires the local board owner")
        );
        Ok(())
    }
    pub(crate) fn message(&self, c: &Connection, id: i64, action: Action) -> Result<String> {
        let channel: Option<String> = c
            .query_row("SELECT channel FROM messages WHERE id=?", [id], |r| {
                r.get(0)
            })
            .optional()?;
        let channel = channel
            .filter(|s| self.allows(Action::Read, s))
            .context(Missing.error("message_not_found", format!("message #{id} not found")))?;
        self.require(action, &channel)?;
        Ok(channel)
    }
    pub(crate) fn channels(&self, c: &Connection, action: Action) -> Result<Option<Vec<String>>> {
        if self.everywhere(action) {
            return Ok(None);
        }
        Ok(Some(
            c.prepare("SELECT name FROM channels")?
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .filter(|s| self.allows(action, s))
                .collect(),
        ))
    }
    pub(crate) fn view(&self, c: &Connection, mut m: crate::Message) -> Result<crate::Message> {
        crate::schemas::correct_snapshot(c, &mut m)?;
        if !self.allows(Action::Read, &m.channel) {
            m.links.clear();
            m.file_notices.clear();
            m.notified.clear();
            m.suppressed.clear();
        } else if m.channel.starts_with("swarms/") && !m.deleted {
            crate::swarm::read_mention_routing(c, &mut m)?;
        }
        if !self.is_trusted()
            && let Some(origin) = &m.origin
        {
            let owner: Option<String> = c
                .query_row(
                    "SELECT owner FROM sync_sources WHERE name=?",
                    [&origin.source],
                    |r| r.get(0),
                )
                .optional()?;
            if self.integration_narrowed()
                || !self.allows(Action::Sync, &m.channel)
                || !self
                    .bound_actor()
                    .zip(owner.as_deref())
                    .is_some_and(|(who, o)| crate::owned_by(o, who))
            {
                m.origin = None;
            }
        }
        if !self.everywhere(Action::Read)
            && let Some(task) = &mut m.task
        {
            let mut visible = Vec::new();
            for id in &task.needs {
                let ch: Option<String> = c
                    .query_row("SELECT channel FROM messages WHERE id=?", [id], |r| {
                        r.get(0)
                    })
                    .optional()?;
                if ch.is_some_and(|s| self.allows(Action::Read, &s)) {
                    visible.push(*id);
                }
            }
            task.needs = visible;
        }
        Ok(m)
    }
    pub(crate) fn blind_on(&self, c: &Connection, id: i64) -> Result<bool> {
        Ok(c.query_row(
            "SELECT EXISTS(SELECT 1 FROM blind_posts WHERE id=?)",
            [id],
            |r| r.get(0),
        )?)
    }
    pub(crate) fn original(&self, c: &Connection, id: i64) -> Result<crate::Message> {
        let raw: String = c.query_row(
            "SELECT message FROM events WHERE cursor=? AND kind='message.posted'",
            [id],
            |r| r.get(0),
        )?;
        let mut m: crate::Message = serde_json::from_str(&raw)?;
        m.links.clear();
        m.file_notices.clear();
        m.notified.clear();
        m.suppressed.clear();
        m.text = crate::prose::redact_exact(&m.text, &m.actor);
        m.origin = None;
        Ok(m)
    }
    pub(crate) fn own_post(&self, c: &Connection, id: i64, actor: &str) -> Result<bool> {
        self.actor(actor)?;
        let target = c
            .query_row("SELECT actor,channel FROM messages WHERE id=?", [id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .optional()?;
        let missing = || Missing.error("message_not_found", format!("message #{id} not found"));
        let (owner, channel) = target.with_context(missing)?;
        let blind = !self.allows(Action::Read, &channel);
        ensure!(
            !blind || (crate::owned_by(&owner, actor) && self.blind_on(c, id)?),
            missing()
        );
        self.require(Action::Post, &channel)?;
        Ok(blind)
    }
}

impl Board {
    pub fn viewer(&self, actor: &str) -> Result<Viewer> {
        crate::validate_actor(actor)?;
        let tx = self.conn.unchecked_transaction()?;
        let a = self.access_on(&tx)?;
        a.actor(actor)?;
        let (principal, profile) = actor
            .split_once('/')
            .map_or((actor, None), |(p, s)| (p, Some(s.to_owned())));
        let mut patterns: BTreeSet<String> = a
            .policy
            .iter()
            .chain(&a.caps)
            .flat_map(|p| p.0.keys().cloned())
            .collect();
        patterns.insert("*".into());
        let mut can = BTreeMap::new();
        for p in patterns {
            let ch = Access::pattern_sample(&p);
            let set: Actions = ALL
                .into_iter()
                .filter(|action| a.allows(*action, &ch))
                .collect();
            if !set.is_empty() || p != "*" {
                can.insert(p, set);
            }
        }
        let author = crate::actors::card_on(&tx, actor)?;
        let v = Viewer {
            actor: actor.into(),
            principal: principal.into(),
            profile,
            handle: author.handle,
            name: author.name,
            avatar: self
                .request_avatar
                .as_ref()
                .map(|(_, s)| s.clone())
                .or(author.avatar),
            can: Rules(can),
            cursor: crate::read::cursor_on(&tx)?,
            owner: a.is_trusted(),
        };
        tx.commit()?;
        Ok(v)
    }
    pub(crate) fn credential_access_on(c: &Connection, id: &str) -> Result<Option<Access>> {
        let row=c.query_row("SELECT actor,scope FROM access_credentials WHERE id=? AND NOT revoked AND (expires IS NULL OR expires>unixepoch())",
            [id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?))).optional()?;
        let Some((actor, scope)) = row else {
            return Ok(None);
        };
        let matched = policy::matching(c, &actor)?;
        let policy = policy::combined(&matched, false);
        let own = policy::combined(&matched, true);
        Ok(Some(Access {
            actor: Some(actor),
            id: Some(id.to_owned()),
            policy,
            own,
            caps: scope
                .iter()
                .map(|v| serde_json::from_str(v))
                .collect::<serde_json::Result<_>>()?,
            extra_cap: false,
        }))
    }
    pub(crate) fn admitted_credential_on(c: &Connection, id: &str) -> Result<Access> {
        let a = Self::credential_access_on(c, id)?
            .context(Forbidden.error("invalid_credential", "credential is revoked or expired"))?;
        ensure!(
            a.own.is_some(),
            Forbidden.error("no_access", "no active grant for this actor")
        );
        Ok(a)
    }
    pub(crate) fn access_on(&self, c: &Connection) -> Result<Access> {
        match &self.session {
            Session::Rejected => Err(Forbidden
                .error("invalid_credential", "connection has no valid credential")
                .into()),
            Session::Owner => Ok(Access::owner()),
            Session::NarrowedOwner { actor, cap } => Ok(Access {
                actor: Some(actor.clone()),
                caps: vec![cap.clone()],
                extra_cap: true,
                ..Access::owner()
            }),
            Session::Credential { id, cap } => {
                let mut access = Self::admitted_credential_on(c, id)?;
                if let Some(cap) = cap {
                    access.caps.push(cap.clone());
                    access.extra_cap = true;
                }
                Ok(access)
            }
        }
    }
    fn session_for(&self, secret: &str, cap: Option<Rules>) -> Result<(Session, String)> {
        ensure!(
            secret.len() <= 512,
            Forbidden.error("invalid_credential", "unknown local credential")
        );
        let hash = Sha256::digest(secret.as_bytes()).to_vec();
        let row=self.conn.query_row("SELECT id,actor FROM access_credentials WHERE digest=? AND NOT revoked AND (expires IS NULL OR expires>unixepoch())",
            [hash],|r|Ok((r.get::<_,String>(0)?,r.get(1)?))).optional()?
            .context(Forbidden.error("invalid_credential","unknown, revoked or expired local credential"))?;
        Ok((Session::Credential { id: row.0, cap }, row.1))
    }
    /// Permanently bind this connection to the fixed actor of an opaque credential.
    pub fn bind_token(&mut self, secret: &str) -> Result<String> {
        ensure!(
            matches!(self.session, Session::Owner),
            Forbidden.error("already_bound", "connection is already bound")
        );
        let attempt = self.session_for(secret, None);
        self.session = Session::Rejected;
        let (session, actor) = attempt?;
        self.session = session;
        self.access_on(&self.conn)?;
        Ok(actor)
    }
    pub fn bind_env(&mut self) -> Result<Option<String>> {
        match std::env::var("BOARD_TOKEN") {
            Ok(v) => Ok(Some(self.bind_token(&v)?)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    /// Attach profile metadata to successful write transactions, not to reads or failed writes.
    pub fn set_write_avatar(&mut self, actor: &str, url: &str) -> Result<()> {
        let (_, profile) = actor
            .split_once('/')
            .context("an avatar needs a named profile")?;
        crate::local::profile_name(profile)?;
        self.access_on(&self.conn)?.actor(actor)?;
        crate::actors::valid_avatar(url)?;
        self.request_avatar = Some((actor.to_owned(), url.to_owned()));
        Ok(())
    }
    pub fn clear_write_avatar(&mut self) {
        self.request_avatar = None;
    }
    /// Execute one request on a serialized owner connection; restricted callers
    /// cannot rebind it. Domain methods refresh the policy inside their own transactions.
    pub fn with_request<T>(
        &mut self,
        secret: Option<&str>,
        owner: &str,
        scope: Option<&str>,
        f: impl FnOnce(&Board, &str) -> Result<T>,
    ) -> Result<T> {
        self.with_profile_request(secret, owner, scope, None, f)
    }
    pub fn with_profile_request<T>(
        &mut self,
        secret: Option<&str>,
        owner: &str,
        scope: Option<&str>,
        profile: Option<&str>,
        f: impl FnOnce(&Board, &str) -> Result<T>,
    ) -> Result<T> {
        ensure!(
            matches!(self.session, Session::Owner),
            Forbidden.error("owner_required", "cannot serve from a bound credential")
        );
        let cap = scope.map(Rules::parse).transpose()?;
        let (mut session, mut actor) = if let Some(s) = secret {
            self.session_for(s, cap)?
        } else if let Some(cap) = cap {
            (
                Session::NarrowedOwner {
                    actor: owner.into(),
                    cap,
                },
                owner.into(),
            )
        } else {
            (Session::Owner, owner.into())
        };
        if let Some(raw) = profile.filter(|s| !s.trim().is_empty()) {
            let (name, avatar) = crate::local::profile_item(raw)?;
            let principal = actor.split('/').next().unwrap_or(&actor);
            let selected = format!("{principal}/{name}");
            ensure!(
                secret.is_none() || selected == actor,
                Forbidden.error("actor_mismatch", "credential is fixed to its issued actor")
            );
            crate::validate_actor(&selected)?;
            if let Session::NarrowedOwner { actor, .. } = &mut session {
                actor.clone_from(&selected);
            }
            self.request_avatar = avatar.map(|a| (selected.clone(), a));
            actor = selected;
        }
        self.session = session;
        let result = self.access_on(&self.conn).and_then(|_| f(self, &actor));
        self.request_avatar = None;
        self.session = Session::Owner;
        result
    }
    pub fn require_owner(&self) -> Result<()> {
        self.access_on(&self.conn)?.trusted()
    }
}
