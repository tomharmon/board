//! A quote is a snapshot, not an instruction to fetch an arbitrary URL.
use anyhow::Result;
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct Link {
    pub url: String,
    pub message: Option<i64>,
    pub author: Option<String>,
    pub text: Option<String>,
    pub reason: Option<String>,
}

pub fn slack_address(raw: &str) -> Option<(String, String, Option<String>)> {
    let u = url::Url::parse(raw).ok()?;
    let h = u.host_str()?;
    if u.scheme() != "https"
        || !(h == "slack.com" || h.ends_with(".slack.com"))
        || u.port_or_known_default() != Some(443)
        || !u.username().is_empty()
        || u.password().is_some()
    {
        return None;
    }
    let mut p = u.path_segments()?;
    if p.next()? != "archives" {
        return None;
    }
    let c = p.next()?;
    let n = p.next()?.strip_prefix('p')?;
    if p.next().is_some()
        || c.len() < 2
        || !c.bytes().all(|b| b.is_ascii_alphanumeric())
        || !(7..=22).contains(&n.len())
        || !n.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let (first, last) = n.split_at(n.len() - 6);
    let thread = u
        .query_pairs()
        .find(|(k, _)| k == "thread_ts")
        .map(|(_, v)| v.into_owned())
        .filter(|v| {
            v.len() <= 26 && v.contains('.') && v.bytes().all(|b| b == b'.' || b.is_ascii_digit())
        });
    Some((c.into(), format!("{first}.{last}"), thread))
}
pub fn prose_links(text: &str) -> Vec<String> {
    static URL: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"https?://[^\s<>"`\[\]()]+"#).unwrap());
    let mut out = Vec::new();
    let mut code = false;
    let mut add = |u: &str| {
        let u = u.trim_end_matches(['.', ',', ';', ':', '!']);
        if u.len() <= 4096 && out.len() < 12 && !out.iter().any(|x| x == u) {
            out.push(u.to_owned());
        }
    };
    for event in Parser::new(text) {
        match event {
            Event::Start(Tag::CodeBlock(_)) => code = true,
            Event::End(TagEnd::CodeBlock) => code = false,
            Event::Start(Tag::Link { dest_url, .. })
                if dest_url.starts_with("https://") || dest_url.starts_with("http://") =>
            {
                add(&dest_url)
            }
            Event::Text(t) if !code => {
                for m in URL.find_iter(&t) {
                    add(m.as_str());
                }
            }
            _ => {}
        }
    }
    out
}
pub(crate) fn on(c: &Connection, id: i64) -> Result<Vec<Link>> {
    let items=c.prepare("SELECT url,target,author,text,reason FROM message_links WHERE message=? ORDER BY ordinal")?
        .query_map([id],|r|Ok(Link{url:r.get(0)?,message:r.get(1)?,author:r.get(2)?,text:r.get(3)?,reason:r.get(4)?}))?.collect::<rusqlite::Result<_>>()?;
    Ok(items)
}
fn store(tx: &Transaction<'_>, id: i64, i: usize, l: &Link) -> Result<()> {
    tx.execute("INSERT OR REPLACE INTO message_links(message,ordinal,url,target,author,text,reason) VALUES(?,?,?,?,?,?,?)",params![id,i as i64,l.url,l.message,l.author,l.text,l.reason])?;
    Ok(())
}
pub(crate) fn capture(tx: &Transaction<'_>, id: i64, channel: &str, text: &str) -> Result<()> {
    let urls = prose_links(text);
    let old = on(tx, id)?;
    if urls.is_empty() && old.is_empty() {
        return Ok(());
    }
    tx.execute("DELETE FROM message_links WHERE message=?", [id])?;
    for (i, url) in urls.iter().enumerate() {
        if let Some(l) = old.iter().find(|x| &x.url == url) {
            store(tx, id, i, l)?;
            continue;
        }
        let slack = slack_address(url);
        let mut local=tx.query_row("SELECT m.id,m.actor,m.text FROM sync_copies c JOIN messages m ON c.message=m.id
            WHERE m.channel=?1 AND m.id!=?2 AND m.deleted=0 AND c.url=?3 ORDER BY c.origin DESC,m.id LIMIT 1",
            params![channel,id,url],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional()?;
        if local.is_none()
            && let Some((remote, external, parent)) = &slack
        {
            // A Slack conversation ID is local to its workspace; the matching timestamp alone
            // cannot authorize attaching a quote from a different workspace.
            let requested = url::Url::parse(url)?;
            let mut stmt=tx.prepare("SELECT m.id,m.actor,m.text,coalesce(c.url,rc.url) FROM sync_copies c
                JOIN messages m ON c.message=m.id JOIN sync_sources s ON s.name=c.source
                LEFT JOIN sync_copies rc ON rc.source=c.source AND rc.thread=c.thread AND rc.message=coalesce(m.re,m.id)
                WHERE m.channel=?1 AND m.id!=?2 AND m.deleted=0 AND s.provider='slack' AND s.remote=?3
                AND c.external=?4 AND (?5 IS NULL OR c.thread=?5) ORDER BY c.origin DESC,m.id")?;
            let mut rows = stmt.query(params![channel, id, remote, external, parent])?;
            while let Some(row) = rows.next()? {
                let recorded: Option<String> = row.get(3)?;
                if recorded
                    .as_deref()
                    .and_then(|s| url::Url::parse(s).ok())
                    .is_some_and(|u| u.host_str() == requested.host_str())
                {
                    local = Some((row.get(0)?, row.get(1)?, row.get(2)?));
                    break;
                }
            }
        }
        let l = if let Some((target, author, text)) = local {
            Link {
                url: url.clone(),
                message: Some(target),
                author: Some(author),
                text: Some(text.chars().take(2000).collect()),
                reason: None,
            }
        } else if slack.is_some() {
            Link {
                url: url.clone(),
                message: None,
                author: None,
                text: None,
                reason: Some("not_resolved".into()),
            }
        } else {
            continue;
        };
        store(tx, id, i, &l)?;
    }
    Ok(())
}
pub(crate) fn enrich(tx: &Transaction<'_>, id: i64, hints: &[Link]) -> Result<bool> {
    let mut changed = false;
    for l in hints.iter().take(12) {
        if slack_address(&l.url).is_none() {
            continue;
        }
        let text = l
            .text
            .as_ref()
            .map(|t| t.chars().take(2000).collect::<String>());
        let author = l
            .author
            .as_ref()
            .map(|a| a.chars().take(120).collect::<String>());
        let reason = if text.is_some() {
            None
        } else {
            l.reason
                .as_deref()
                .filter(|v| matches!(*v, "not_public" | "not_in_channel" | "unavailable"))
        };
        // A partial edit must not erase a good unfurl or override a same-channel Board snapshot.
        changed|=tx.execute("UPDATE message_links SET author=?3,text=?4,reason=?5 WHERE message=?1 AND url=?2 AND target IS NULL AND (text IS NULL OR ?4 IS NOT NULL) AND (author IS NOT ?3 OR text IS NOT ?4 OR reason IS NOT ?5)",
            params![id,l.url,author,text,reason])?>0;
    }
    Ok(changed)
}
