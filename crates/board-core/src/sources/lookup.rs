//! Public, read-authorized projections. Never contact a provider or expose a raw delivery.
use super::*;
use crate::access::{Access, Action};
use rusqlite::Params;
use url::Url;

#[derive(Clone, Debug, Serialize)]
pub struct SyncTarget {
    pub source: String,
    pub target: String,
    pub thread: Option<String>,
    pub external: Option<String>,
    pub url: Option<String>,
    pub state: String,
}
#[derive(Debug, Serialize)]
pub struct SyncTargets {
    pub cursor: i64,
    pub message: i64,
    pub root: i64,
    pub targets: Vec<SyncTarget>,
}
#[derive(Debug, Serialize)]
pub struct LocatedLink {
    pub cursor: i64,
    pub message: i64,
    pub root: i64,
}
fn parse_url(raw: &str) -> Option<Url> {
    if raw.len() > 4096 || raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    (matches!(url.scheme(), "https" | "http")
        && url.has_host()
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(url)
}
fn displayable(raw: &str) -> bool {
    parse_url(raw).is_some_and(|u| {
        u.query().is_none()
            || (crate::links::slack_address(raw).is_some()
                && u.query_pairs().all(|(k, v)| match k.as_ref() {
                    "thread_ts" => {
                        !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit() || b == b'.')
                    }
                    "cid" => !v.is_empty() && v.bytes().all(|b| b.is_ascii_alphanumeric()),
                    _ => false,
                }))
    })
}
fn public_id(raw: &str) -> bool {
    !raw.contains("://") || displayable(raw)
}
fn gh_address(url: &Url) -> Option<(String, &str, String)> {
    if url.scheme() != "https" || url.host_str() != Some("github.com") || url.port().is_some() {
        return None;
    }
    let mut path = url.path_segments()?;
    let owner = path.next()?;
    let repo = path.next()?;
    let kind = path.next()?;
    let number = path.next()?;
    if owner.is_empty()
        || repo.is_empty()
        || !matches!(kind, "pull" | "issues")
        || number.is_empty()
        || !number.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    match path.next() {
        Some("files" | "commits" | "checks") if kind == "pull" => (),
        None | Some("") => (),
        _ => return None,
    };
    if path.next().is_some() {
        return None;
    }
    let copy = match url.fragment() {
        Some(f) => {
            let (family, id) = if let Some(id) = f.strip_prefix("issuecomment-") {
                ("comment", id)
            } else if let Some(id) = f.strip_prefix("discussion_r").filter(|_| kind == "pull") {
                ("inline", id)
            } else {
                (
                    "review",
                    f.strip_prefix("pullrequestreview-")
                        .filter(|_| kind == "pull")?,
                )
            };
            if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            format!("{family}:{id}")
        }
        None => "issue".into(),
    };
    Some((format!("{owner}/{repo}"), number, copy))
}

fn find(
    c: &Connection,
    a: &Access,
    sql: &str,
    args: impl Params,
    host: Option<&str>,
) -> Result<Option<(i64, i64)>> {
    let mut stmt = c.prepare(sql)?;
    let mut rows = stmt.query(args)?;
    let mut out = None;
    while let Some(row) = rows.next()? {
        let channel: String = row.get(2)?;
        if !a.allows(Action::Read, &channel) {
            continue;
        }
        if let Some(expected) = host {
            let recorded: Option<String> = row.get(3)?;
            if !recorded
                .as_deref()
                .and_then(parse_url)
                .is_some_and(|u| u.host_str() == Some(expected))
            {
                continue;
            }
        }
        let id: i64 = row.get(0)?;
        let root: i64 = row.get(1)?;
        if let Some((_, previous)) = out {
            ensure!(
                previous == root,
                Kind::Conflict.error(
                    "ambiguous_link",
                    "the link identifies multiple readable conversations"
                )
            );
            if id != root {
                continue;
            }
        }
        out = Some((id, root));
    }
    Ok(out)
}
// An unresolved external send takes precedence over later bookkeeping. Otherwise,
// report the latest text delivery, not file or reaction jobs for the same message.
const TARGETS: &str = "SELECT t.source,s.remote,t.thread,c.external,c.url,
    (SELECT q.state FROM sync_queue q WHERE q.source=t.source AND q.direction='out' AND q.thread=t.thread
        AND json_extract(q.payload,'$.message')=?1 AND json_extract(q.payload,'$.event') IN ('thread.start','message.posted','message.edited','message.deleted')
        ORDER BY (q.state='uncertain') DESC,q.id DESC LIMIT 1),
    EXISTS(SELECT 1 FROM private_messages WHERE id IN (?1,?2)),
    EXISTS(SELECT 1 FROM sync_starts st JOIN sync_queue start ON start.id=st.item
        WHERE st.source=t.source AND st.root=t.root AND start.thread=t.thread AND start.state!='done')
    FROM sync_threads t JOIN sync_sources s ON s.name=t.source LEFT JOIN sync_copies c
        ON c.source=t.source AND c.thread=t.thread AND c.message=?1
    WHERE t.root=?2 ORDER BY t.source,t.thread";

fn target_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<SyncTarget> {
    let source: String = r.get(0)?;
    let remote: String = r.get(1)?;
    let thread: String = r.get(2)?;
    let external: Option<String> = r.get(3)?;
    let url: Option<String> = r.get(4)?;
    let state: Option<String> = r.get(5)?;
    let local_only: bool = r.get(6)?;
    let provisional: bool = r.get(7)?;
    let state = match state.as_deref() {
        _ if local_only && external.is_none() => "local_only",
        Some(s @ ("pending" | "leased" | "sending" | "uncertain" | "failed")) => s,
        _ if external.is_some() => "confirmed",
        Some("skipped") => "skipped",
        _ => "not_sent",
    };
    Ok(SyncTarget {
        target: if public_id(&remote) {
            remote
        } else {
            source.clone()
        },
        source,
        thread: (!provisional && public_id(&thread)).then_some(thread),
        external: external.filter(|s| public_id(s)),
        url: url.filter(|s| displayable(s)),
        state: state.to_owned(),
    })
}
impl Board {
    pub fn sync_targets(&self, message: i64) -> Result<SyncTargets> {
        let tx = self.conn.unchecked_transaction()?;
        let a = self.access_on(&tx)?;
        a.message(&tx, message, Action::Read)?;
        let root = tx.query_row(
            "SELECT coalesce(re,id) FROM messages WHERE id=?",
            [message],
            |r| r.get(0),
        )?;
        let targets = tx
            .prepare(TARGETS)?
            .query_map(params![message, root], target_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let cursor = crate::read::cursor_on(&tx)?;
        tx.commit()?;
        Ok(SyncTargets {
            message,
            root,
            targets,
            cursor,
        })
    }

    pub fn locate_link(&self, raw: &str) -> Result<LocatedLink> {
        let missing = || {
            Kind::Missing.error(
                "message_not_found",
                "no readable board message for this link",
            )
        };
        let tx = self.conn.unchecked_transaction()?;
        let a = self.access_on(&tx)?;
        let url = parse_url(raw).context(missing())?;
        let mut hit = None;
        if let Some((repo, number, ext)) = gh_address(&url) {
            hit = if ext == "issue" {
                find(
                    &tx,
                    &a,
                    "SELECT coalesce(c.message,m.id),m.id,m.channel FROM sync_sources s JOIN sync_threads t ON t.source=s.name JOIN messages m ON m.id=t.root LEFT JOIN sync_copies c ON c.source=t.source AND c.thread=t.thread AND c.external='issue' WHERE s.provider='github' AND s.remote=?1 COLLATE NOCASE AND t.thread=?2 ORDER BY m.id",
                    params![repo, number],
                    None,
                )?
            } else {
                find(
                    &tx,
                    &a,
                    "SELECT m.id,coalesce(m.re,m.id),m.channel FROM sync_sources s JOIN sync_copies c ON c.source=s.name JOIN messages m ON m.id=c.message WHERE s.provider='github' AND s.remote=?1 COLLATE NOCASE AND c.thread=?2 AND c.external=?3 ORDER BY m.id",
                    params![repo, number, ext],
                    None,
                )?
            };
        }
        if hit.is_none() {
            hit = find(
                &tx,
                &a,
                "SELECT m.id,coalesce(m.re,m.id),m.channel FROM sync_copies c JOIN messages m ON m.id=c.message WHERE c.url=? ORDER BY m.id",
                [raw],
                None,
            )?;
        }
        if hit.is_none()
            && let Some((channel, ts, parent)) = crate::links::slack_address(raw)
        {
            // Slack channel IDs are workspace-local. Do not join to an unverified hostname
            // simply because its path looks like a known channel and timestamp.
            hit = find(
                &tx,
                &a,
                "SELECT m.id,coalesce(m.re,m.id),m.channel,coalesce(c.url,rc.url) FROM sync_sources s JOIN sync_copies c ON c.source=s.name JOIN messages m ON m.id=c.message LEFT JOIN sync_copies rc ON rc.source=c.source AND rc.thread=c.thread AND rc.message=coalesce(m.re,m.id) WHERE s.provider='slack' AND s.remote=?1 AND c.external=?2 AND (?3 IS NULL OR c.thread=?3) ORDER BY m.id",
                params![channel, ts, parent],
                url.host_str(),
            )?;
        }
        let (message, root) = hit.context(missing())?;
        let cursor = crate::read::cursor_on(&tx)?;
        tx.commit()?;
        Ok(LocatedLink {
            message,
            root,
            cursor,
        })
    }
}
