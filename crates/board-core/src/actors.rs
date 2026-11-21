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
      CREATE TABLE IF NOT EXISTS actor_avatars(id TEXT PRIMARY KEY,url TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS exact_mentions(message_id INTEGER NOT NULL REFERENCES messages(id),actor TEXT NOT NULL,PRIMARY KEY(message_id,actor));
      CREATE INDEX IF NOT EXISTS exact_mention_actor ON exact_mentions(actor,message_id);
      CREATE TABLE IF NOT EXISTS composer_drafts(actor TEXT NOT NULL,scope TEXT NOT NULL,body TEXT NOT NULL,channel TEXT NOT NULL,reply_to INTEGER,PRIMARY KEY(actor,scope));")?;
    crate::drafts::init(c)?;
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
pub struct MentionActor {
    pub id: String,
    pub handle: String,
    pub name: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MentionResolution {
    pub written: String,
    pub actor: Option<MentionActor>,
    pub candidates: Vec<MentionActor>,
}
impl MentionResolution {
    fn unset(&mut self) {
        if let Some(p) = self.actor.take() {
            self.candidates = vec![p];
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Draft {
    pub body: String,
    pub channel: String,
    pub reply_to: Option<i64>,
}

const CARDS:&str="SELECT k.id,p.handle,p.name,coalesce(p.verified,0),
  coalesce(sp.source,(SELECT cp.source FROM sync_copies cp JOIN messages seen ON seen.id=cp.message WHERE cp.origin AND seen.actor=k.id LIMIT 1)),
  sp.name,coalesce(sp.avatar,av.url) FROM known k LEFT JOIN actor_profiles p ON p.id=k.id
  LEFT JOIN sync_profiles sp ON sp.actor=k.id LEFT JOIN actor_avatars av ON av.id=k.id";
fn imported_handle(source: &str, name: Option<&str>, fallback: &str) -> String {
    static MARKS: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\p{M}+").expect("Unicode category"));
    fn slug(s: &str) -> String {
        let decomposed = icu_normalizer::DecomposingNormalizerBorrowed::new_nfkd()
            .normalize_iter(s.chars())
            .collect::<String>();
        let mut out = String::new();
        let mut space = false;
        for c in MARKS
            .replace_all(&decomposed, "")
            .chars()
            .flat_map(char::to_lowercase)
        {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                if space {
                    out.push('-');
                    space = false;
                }
                out.push(c);
            } else {
                space = true;
            }
        }
        out.trim_matches(['.', '_', '-']).to_owned()
    }
    let mut leaf = name.map_or_else(String::new, slug);
    if leaf.is_empty() {
        leaf = slug(fallback);
    }
    let room = 128usize.saturating_sub(source.len() + 1);
    if leaf.len() > room {
        leaf.truncate(room);
        leaf.truncate(leaf.trim_end_matches(['.', '_', '-']).len());
    }
    format!("{source}/{leaf}")
}
fn card(r: &rusqlite::Row<'_>) -> rusqlite::Result<ActorEntry> {
    let id: String = r.get(0)?;
    let source: Option<String> = r.get(4)?;
    let external = source.is_some();
    let profile = id.split_once('/');
    let remote_name: Option<String> = r.get(5)?;
    let default_handle = if let Some(source) = &source {
        imported_handle(
            source,
            remote_name.as_deref(),
            profile.map_or(id.as_str(), |p| p.1),
        )
    } else {
        id.clone()
    };
    let name = remote_name
        .or(r.get(2)?)
        .unwrap_or_else(|| profile.map_or(id.as_str(), |(_, s)| s).to_owned());
    Ok(ActorEntry {
        handle: r.get::<_, Option<String>>(1)?.unwrap_or(default_handle),
        name,
        kind: if external {
            "external"
        } else if profile.is_some() {
            "agent"
        } else {
            "person"
        }
        .into(),
        via: source.or_else(|| profile.map(|(p, _)| p.to_owned())),
        avatar: r.get(6)?,
        verified: !external && r.get(3)?,
        local: !external,
        id,
    })
}
pub(crate) fn card_on(c: &Connection, id: &str) -> Result<ActorEntry> {
    Ok(
        c.prepare_cached(&format!("WITH known(id) AS (VALUES (?1)) {CARDS}"))?
            .query_row([id], card)?,
    )
}
pub(crate) fn valid_avatar(s: &str) -> Result<()> {
    let uri = url::Url::parse(s).context("invalid avatar URL")?;
    ensure!(
        s.len() <= 2048
            && !s.chars().any(|c| c.is_control() || c.is_whitespace())
            && (uri.scheme() == "https"
                || (uri.scheme() == "http"
                    && matches!(uri.host_str(), Some("localhost" | "127.0.0.1"))))
            && uri.host().is_some()
            && uri.username().is_empty()
            && uri.password().is_none(),
        "avatar must be HTTPS, or HTTP on localhost, without credentials"
    );
    Ok(())
}

impl Board {
    pub fn actor_directory(&self, query: Option<&str>) -> Result<Vec<ActorEntry>> {
        Ok(self.actor_directory_page(query, &[])?.1)
    }
    /// A directory response and its event cursor come from the same read transaction.
    /// Unknown and inaccessible requested IDs are indistinguishable.
    pub fn actor_directory_page(
        &self,
        query: Option<&str>,
        ids: &[String],
    ) -> Result<(i64, Vec<ActorEntry>)> {
        ensure!(
            ids.len() <= 100,
            Invalid.error("invalid_actor_ids", "at most 100 actor IDs")
        );
        for id in ids {
            validate_actor(id)?;
        }
        let tx = self.conn.unchecked_transaction()?;
        let a = self.access_on(&tx)?;
        let rows = Self::directory_selected(&tx, &a, query, ids)?;
        let cursor = crate::read::cursor_on(&tx)?;
        tx.commit()?;
        Ok((cursor, rows))
    }
    pub(crate) fn actor_directory_on(
        tx: &Connection,
        a: &crate::access::Access,
        query: Option<&str>,
    ) -> Result<Vec<ActorEntry>> {
        Self::directory_selected(tx, a, query, &[])
    }
    fn directory_selected(
        tx: &Connection,
        a: &crate::access::Access,
        query: Option<&str>,
        ids: &[String],
    ) -> Result<Vec<ActorEntry>> {
        let channels = a
            .channels(tx, Action::Read)?
            .map(|v| serde_json::to_string(&v))
            .transpose()?;
        let known = if ids.is_empty() {
            "SELECT actor FROM events WHERE ?3 IS NULL UNION SELECT id FROM actor_profiles UNION SELECT actor FROM sync_profiles UNION SELECT ?2 WHERE ?2 IS NOT NULL"
        } else {
            "SELECT DISTINCT w.value FROM json_each(?3) w WHERE w.value=?2 OR EXISTS(SELECT 1 FROM events WHERE actor=w.value)
                OR EXISTS(SELECT 1 FROM actor_profiles WHERE id=w.value) OR EXISTS(SELECT 1 FROM sync_profiles WHERE actor=w.value)"
        };
        let wanted = if ids.is_empty() {
            None
        } else {
            Some(serde_json::to_string(ids)?)
        };
        let sql=format!("WITH known(id) AS ({known}),
           readable(id,actor) AS (SELECT id,actor FROM messages WHERE channel IN (SELECT value FROM json_each(?1))),
           visible(id) AS (SELECT actor FROM readable UNION SELECT r.actor FROM reactions r JOIN readable m ON m.id=r.message_id
           UNION SELECT e.actor FROM exact_mentions e JOIN readable m ON e.message_id=m.id UNION SELECT ?2 WHERE ?2 IS NOT NULL)
           {CARDS} WHERE ?1 IS NULL OR k.id IN (SELECT id FROM visible)");
        let rows = tx
            .prepare(&sql)?
            .query_map(params![channels, a.bound_actor(), wanted], card)?
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
        Ok(rows)
    }
    /// Change only one's own local presentation. Neither field is an identity credential.
    pub fn set_actor_profile(&self, actor: &str, handle: &str, name: &str) -> Result<()> {
        self.set_actor_profile_with_avatar(actor, handle, name, None)
    }
    /// Omitting the avatar preserves it. An explicit empty string removes it.
    pub fn set_actor_profile_with_avatar(
        &self,
        actor: &str,
        handle: &str,
        name: &str,
        avatar: Option<&str>,
    ) -> Result<()> {
        validate_actor(actor)?;
        if let Some(url) = avatar.filter(|s| !s.is_empty()) {
            valid_avatar(url)?;
        }
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
            let profile_changed = tx.execute("INSERT INTO actor_profiles VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE SET handle=excluded.handle,name=excluded.name,verified=excluded.verified
               WHERE handle<>excluded.handle OR name<>excluded.name OR verified<>excluded.verified",params![actor,handle,name,a.credential_id().is_some()])? != 0;
            let avatar_changed = match avatar{
                None => false,
                Some("")=> tx.execute("DELETE FROM actor_avatars WHERE id=?", [actor])? != 0,
                Some(url)=> tx.execute("INSERT INTO actor_avatars VALUES(?,?) ON CONFLICT(id) DO UPDATE SET url=excluded.url WHERE url<>excluded.url", params![actor,url])? != 0,
            };
            if profile_changed || avatar_changed {
                Self::event_tx(tx,"actor.updated",actor,None,serde_json::json!({"id":actor}))?;
            }Ok(())})
    }
    pub(crate) fn draft_scope(
        &self,
        c: &Connection,
        actor: &str,
        scope: &str,
    ) -> Result<Option<i64>> {
        validate_actor(actor)?;
        ensure!(
            !actor.starts_with("source:"),
            Invalid.error("invalid_draft", "imported identities do not own composers")
        );
        let a = self.access_on(c)?;
        a.actor(actor)?;
        if matches!(scope, "new" | "tasks") {
            return Ok(None);
        }
        if let Some(channel) = scope.strip_prefix("channel:") {
            validate_channel(channel)?;
            ensure!(
                a.allows(Action::Read, channel) || a.allows(Action::Post, channel),
                Missing.error("draft_not_found", "draft not found")
            );
            return Ok(None);
        }
        let id = scope
            .strip_prefix("thread:")
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|v| *v > 0)
            .context(Invalid.error(
                "invalid_draft",
                "expected new, tasks, channel:NAME or thread:ID",
            ))?;
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
                ensure!(d.body.encode_utf16().count()<=10_000 && !d.body.contains('\0'),"draft exceeds message limits");
                ensure!(scope.strip_prefix("channel:").is_none_or(|ch| ch == d.channel),
                    Invalid.error("invalid_draft", "draft destination differs from its channel"));
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

pub(crate) fn settle(
    c: &Connection,
    a: &crate::access::Access,
    actor: &str,
    thread: Option<i64>,
    text: &str,
) -> Result<(String, Vec<MentionResolution>)> {
    use std::collections::{BTreeMap, BTreeSet};
    let tokens: Vec<_> = crate::prose::tokens(text)
        .into_iter()
        .filter(|t| !t.exact)
        .collect();
    if tokens.is_empty() {
        return Ok((text.to_owned(), vec![]));
    }
    let people = Board::actor_directory_on(c, a, None)?;
    let writers = if let Some(id) = thread
        && a.message(c, id, Action::Read).is_ok()
    {
        c.prepare(
            "WITH root AS (SELECT coalesce(re,id) FROM messages WHERE id=?1)
            SELECT DISTINCT actor FROM messages WHERE coalesce(re,id)=(SELECT * FROM root)",
        )?
        .query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<BTreeSet<String>>>()?
    } else {
        BTreeSet::new()
    };
    let mut resolutions = Vec::<MentionResolution>::new();
    let mut handles = BTreeMap::<String, usize>::new();
    let mut rewritten = String::with_capacity(text.len());
    let mut at = 0;
    let card = |p: &ActorEntry| MentionActor {
        id: p.id.clone(),
        handle: p.handle.clone(),
        name: p.name.clone(),
    };
    for t in tokens {
        let key = t.value.to_ascii_lowercase();
        let index = *handles.entry(key.clone()).or_insert_with(|| {
            let eligible = || {
                people
                    .iter()
                    .filter(|p| !p.id.contains(']') && !p.id.chars().any(char::is_whitespace))
            };
            let full: Vec<_> = eligible()
                .filter(|p| p.handle.eq_ignore_ascii_case(&key))
                .collect();
            let board: Vec<_> = full.iter().copied().filter(|p| p.local).collect();
            let choices = if board.is_empty() { &full } else { &board };
            let selected = if choices.len() == 1 {
                choices.first().copied()
            } else {
                let mut thread = choices.iter().copied().filter(|p| writers.contains(&p.id));
                thread.next().filter(|_| thread.next().is_none())
            };
            let candidates = if selected.is_some() {
                vec![]
            } else if full.is_empty() && !key.contains('/') {
                eligible()
                    .filter(|p| {
                        p.handle
                            .split_once('/')
                            .is_some_and(|(_, short)| short.eq_ignore_ascii_case(&key))
                    })
                    .map(card)
                    .collect()
            } else {
                full.iter().map(|p| card(p)).collect()
            };
            let i = resolutions.len();
            resolutions.push(MentionResolution {
                written: text[t.range.clone()].into(),
                actor: selected.map(card),
                candidates,
            });
            i
        });
        rewritten.push_str(&text[at..t.range.start]);
        if let Some(person) = &resolutions[index].actor {
            rewritten.push_str("@[");
            rewritten.push_str(&person.id);
            rewritten.push(']');
        } else {
            rewritten.push_str(&text[t.range.clone()]);
        }
        at = t.range.end;
    }
    rewritten.push_str(&text[at..]);
    if rewritten.encode_utf16().count() > 10_000 && text.encode_utf16().count() <= 10_000 {
        for m in &mut resolutions {
            m.unset();
        }
        rewritten = text.to_owned();
    }
    a.actor(actor)?;
    Ok((rewritten, resolutions))
}
pub(crate) fn write_view(
    mut m: crate::Message,
    expected: &str,
    mut resolutions: Vec<MentionResolution>,
    blind: bool,
) -> crate::Message {
    if blind {
        resolutions.retain_mut(|r| {
            r.candidates.retain(|p| p.id == m.actor);
            if r.actor.as_ref().is_some_and(|p| p.id != m.actor) {
                r.actor = None;
            }
            true
        });
    }
    if expected != m.text && !resolutions.is_empty() {
        let ids: std::collections::BTreeSet<_> = crate::prose::tokens(&m.text)
            .into_iter()
            .filter(|t| t.exact)
            .map(|t| t.value)
            .collect();
        for r in &mut resolutions {
            if r.actor
                .as_ref()
                .is_some_and(|p| !ids.contains(p.id.as_str()))
            {
                r.unset();
            }
        }
    }
    m.mentions = resolutions;
    m
}

pub(crate) fn mention_terms(
    c: &Connection,
    a: &crate::access::Access,
    query: &str,
) -> Result<Vec<String>> {
    let term = crate::prose::query(query);
    let mut terms = vec![term.clone()];
    if let Some(id) = crate::prose::exact_selector(query) {
        if a.everywhere(Action::Read) || a.bound_actor() == Some(id) {
            terms.push(crate::prose::term(&card_on(c, id)?.handle));
        }
        terms.sort();
        terms.dedup();
        return Ok(terms.into_iter().map(|t| format!("@[{t}]")).collect());
    }
    if a.everywhere(Action::Read) {
        for person in Board::actor_directory_on(c, a, Some(&term))? {
            let id = crate::prose::term(&person.id);
            let handle = crate::prose::term(&person.handle);
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

#[derive(Default)]
pub(crate) struct Authors {
    pub ids: Vec<String>,
    pub families: Vec<String>,
}
impl Authors {
    pub fn matches(&self, actor: &str) -> bool {
        self.ids.iter().any(|id| id == actor)
            || actor
                .split_once('/')
                .is_some_and(|(principal, _)| self.families.iter().any(|id| id == principal))
    }
}
pub(crate) fn author_filter(
    c: &Connection,
    a: &crate::access::Access,
    query: &str,
) -> Result<Authors> {
    if let Some(id) = crate::prose::exact_selector(query) {
        return Ok(Authors {
            ids: vec![id.to_owned()],
            families: vec![],
        });
    }
    let q = query.trim_start_matches('@');
    let mut cards = Board::actor_directory_on(c, a, Some(q))?;
    // A stable ID is unambiguous even when someone else has claimed that spelling as a handle.
    let id_wins = cards.iter().any(|p| p.id == q);
    cards.retain(|p| {
        if id_wins {
            p.id == q
        } else {
            p.handle.eq_ignore_ascii_case(q)
        }
    });
    let mut out = Authors::default();
    for card in cards {
        if card.kind == "person" {
            out.families.push(card.id.clone());
        }
        out.ids.push(card.id);
    }
    // A person's first visible author may be a profile, and not the person themselves.
    if out.ids.is_empty() {
        out.ids.push(q.to_owned());
        if !q.contains('/') {
            out.families.push(q.to_owned());
        }
    }
    Ok(out)
}
