//! Explicitly-invoked clients for three optional providers. Board's worker and HTTP server never call these clients.
mod github;
mod intake;
mod linear;
mod media;
mod slack;
pub use intake::ingest;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result, bail, ensure};
use board_local::{Board, sources::*};
use chrono::{DateTime, Utc};
use reqwest::{Method, blocking::Client};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    io::Read,
    time::Duration,
};

pub struct Connector {
    http: Client,
    base: String,
    token: Option<String>,
    provider: Provider,
}
#[derive(Debug, Serialize, Default)]
pub struct PollReport {
    pub threads: usize,
    pub records: usize,
    pub applied: usize,
    pub failed: usize,
}
#[derive(Debug)]
struct HttpStatus(u16, Option<i64>);
impl std::fmt::Display for HttpStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "provider returned HTTP {}", self.0)
    }
}
impl std::error::Error for HttpStatus {}

fn s<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str()
        .with_context(|| format!("provider response missing {k}"))
}
fn id(v: &Value, k: &str) -> Result<String> {
    if let Some(x) = v[k].as_u64() {
        Ok(x.to_string())
    } else {
        Ok(s(v, k)?.to_string())
    }
}
fn arr<'a>(v: &'a Value, k: &str) -> Result<&'a Vec<Value>> {
    v[k].as_array()
        .with_context(|| format!("provider response missing list {k}"))
}
fn seg(x: &str) -> String {
    percent_encoding::utf8_percent_encode(x, percent_encoding::NON_ALPHANUMERIC).to_string()
}
fn key(input: &Import) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(input).expect("serializable normalized event"))
    )
}
fn envelope(mut import: Import, raw: Value) -> Envelope {
    let profile = raw
        .get("user")
        .filter(|v| v.is_object())
        .or_else(|| raw.get("creator").filter(|v| v.is_object()))
        .or_else(|| raw.get("user_profile"));
    if let Some(p) = profile
        && let Some(name) = p["name"]
            .as_str()
            .or(p["login"].as_str())
            .or(p["display_name"].as_str())
            .filter(|s| !s.is_empty())
    {
        import.profile = Some(AuthorInfo {
            name: name.to_owned(),
            avatar: p["avatar_url"]
                .as_str()
                .or(p["avatarUrl"].as_str())
                .map(str::to_owned),
        });
    }
    if let Change::Upsert { text, .. } = &mut import.change
        && text.chars().count() > 10_000
    {
        let marker = "\n[truncated; original in connector inbox]";
        *text = text.chars().take(10_000 - marker.len()).collect::<String>() + marker;
    }
    Envelope {
        key: key(&import),
        import,
        raw,
    }
}
fn stamp(v: &Value, k: &str) -> Result<Option<DateTime<Utc>>> {
    v[k].as_str()
        .map(|s| s.parse().context("invalid provider timestamp"))
        .transpose()
}
fn upsert(
    thread: &str,
    external: String,
    author: String,
    parent: Option<String>,
    text: String,
) -> Import {
    Import {
        thread: thread.into(),
        external,
        author,
        profile: None,
        parent,
        url: None,
        updated_at: None,
        change: Change::Upsert {
            text,
            labels: BTreeMap::new(),
        },
    }
}
fn labels(m: &mut Import, fields: &[(&str, &str)]) {
    if let Change::Upsert { labels, .. } = &mut m.change {
        for (k, v) in fields {
            labels.insert((*k).into(), (*v).into());
        }
    }
}
fn property(m: &mut Import, prefix: &str, name: &str) {
    let slug = name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    let slug = slug.trim_matches('-');
    if !slug.is_empty() {
        let key = format!(
            "{prefix}.{}",
            slug.chars().take(63 - prefix.len()).collect::<String>()
        );
        labels(m, &[(&key, "")]);
    }
}
fn content(prefix: &str, body: &Value) -> String {
    let body = body.as_str().unwrap_or("").trim();
    if body.is_empty() {
        prefix.into()
    } else if prefix.is_empty() {
        body.into()
    } else {
        format!("{prefix}\n\n{body}")
    }
}
fn root_text(d: &Delivery) -> (&str, String) {
    if d.event == "message.deleted" {
        return ("[deleted on Board]", String::new());
    }
    let mut lines = d.text.lines();
    (
        lines
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("Board thread"),
        lines.collect::<Vec<_>>().join("\n"),
    )
}
fn order(items: Vec<Envelope>) -> Result<Vec<Envelope>> {
    // Some providers return newest-first, including children ahead of their parents.
    let (mut pending, reactions): (Vec<_>, Vec<_>) = items
        .into_iter()
        .partition(|e| !matches!(e.import.change, Change::React { .. }));
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    while !pending.is_empty() {
        let before = pending.len();
        pending.retain(|e| {
            let m = &e.import;
            if m.parent.as_ref().is_none_or(|p| seen.contains(p)) {
                if seen.insert(m.external.clone()) {
                    result.push(e.clone());
                }
                false
            } else {
                true
            }
        });
        ensure!(
            pending.len() != before,
            "provider returned a missing or cyclic comment parent"
        );
    }
    result.extend(reactions);
    Ok(result)
}
fn endpoint(provider: &Provider) -> Result<&'static str> {
    match provider {
        Provider::Github => Ok("https://api.github.com"),
        Provider::Slack => Ok("https://slack.com/api"),
        Provider::Linear => Ok("https://api.linear.app"),
        Provider::Local => bail!("a local source uses inbox import, not a provider connection"),
    }
}
impl Connector {
    /// Preview an explicitly configured source; neither a read nor a channel join creates Board records.
    pub fn preview(&self, b: &Board, who: &str, name: &str, thread: Option<&str>) -> Result<Value> {
        b.require_owner()?;
        let src = b.source(who, name)?;
        match (&self.provider, thread) {
            (Provider::Github, Some(t)) => Ok(
                json!({"records":order(self.github_read(&src,t)?)?.into_iter().map(|v|v.import).collect::<Vec<_>>()}),
            ),
            (Provider::Slack, Some(t)) => Ok(
                json!({"records":order(self.slack_read(&src,t)?)?.into_iter().map(|v|v.import).collect::<Vec<_>>()}),
            ),
            (Provider::Linear, Some(t)) => Ok(
                json!({"records":order(self.linear_read(&src,t)?)?.into_iter().map(|v|v.import).collect::<Vec<_>>()}),
            ),
            (Provider::Github, None) => Ok(json!({"threads":self.github_threads(&src,None)?})),
            (Provider::Slack, None) => Ok(json!({"threads":self.slack_threads(&src)?})),
            (Provider::Linear, None) => Ok(json!({"threads":self.linear_threads(&src,None)?})),
            _ => bail!("not a network source"),
        }
    }
    pub fn from_env(source: &Source) -> Result<Self> {
        let default = match source.provider {
            Provider::Github => "BOARD_GITHUB_TOKEN",
            Provider::Slack => "BOARD_SLACK_TOKEN",
            Provider::Linear => "BOARD_LINEAR_TOKEN",
            Provider::Local => bail!("local sources do not connect to the network"),
        };
        let name = format!(
            "BOARD_SYNC_{}_TOKEN",
            source.name.replace('-', "_").to_ascii_uppercase()
        );
        let token = std::env::var(&name)
            .ok()
            .or_else(|| std::env::var(default).ok())
            .filter(|x| !x.is_empty());
        ensure!(
            token.is_some() || source.provider == Provider::Github,
            "set {name} or {default} for this connector"
        );
        Self::new(
            source.provider.clone(),
            endpoint(&source.provider)?.into(),
            token,
        )
    }
    fn new(provider: Provider, base: String, token: Option<String>) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(25))
            .connect_timeout(Duration::from_secs(8))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("local-board/0.1")
            .build()?;
        Ok(Self {
            http,
            base,
            token,
            provider,
        })
    }
    fn req(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        payload: Option<&Value>,
    ) -> Result<Value> {
        let mut req = self.http.request(method, format!("{}{path}", self.base));
        if !query.is_empty() {
            req = req.query(query);
        }
        if let Some(t) = &self.token {
            req = if self.provider == Provider::Linear {
                req.header("Authorization", t)
            } else {
                req.bearer_auth(t)
            };
        }
        if self.provider == Provider::Github {
            req = req
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28");
        }
        if let Some(p) = payload {
            req = req.json(p);
        }
        let response = req.send().map_err(|e| {
            anyhow::anyhow!(
                "provider connection failed: {}",
                if e.is_timeout() {
                    "timeout"
                } else {
                    "transport unavailable"
                }
            )
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(HttpStatus(
                status.as_u16(),
                response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok()?.parse().ok()),
            )
            .into());
        }
        let mut buf = Vec::new();
        response.take(8 * 1024 * 1024 + 1).read_to_end(&mut buf)?;
        ensure!(buf.len() <= 8 * 1024 * 1024, "provider page exceeds 8 MiB");
        if buf.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&buf).context("invalid provider JSON")
    }
    pub fn pull(
        &self,
        board: &Board,
        actor: &str,
        name: &str,
        thread: Option<&str>,
    ) -> Result<PollReport> {
        let source = board.source(actor, name)?;
        ensure!(source.provider == self.provider, "provider mismatch");
        let since = board.source_checkpoint(actor, name, "last_scan")?;
        let start = Utc::now().to_rfc3339();
        let threads = match thread {
            Some(t) => vec![t.to_string()],
            None => match source.provider {
                Provider::Github => self.github_threads(&source, since.as_deref())?,
                Provider::Slack => self.slack_threads(&source)?,
                Provider::Linear => self.linear_threads(&source, since.as_deref())?,
                Provider::Local => bail!("not a network source"),
            },
        };
        let mut report = PollReport::default();
        for t in threads {
            let events = match source.provider {
                Provider::Github => self.github_read(&source, &t)?,
                Provider::Slack => self.slack_read(&source, &t)?,
                Provider::Linear => self.linear_read(&source, &t)?,
                Provider::Local => unreachable!(),
            };
            let items = order(events)?;
            for batch in items.chunks(200) {
                board.enqueue_imports(actor, name, batch, None)?;
            }
            report.records += items.len();
            report.threads += 1;
        }
        if thread.is_none() {
            board.enqueue_imports(
                actor,
                name,
                &[],
                Some(&Checkpoint {
                    stream: "last_scan".into(),
                    expected: since,
                    next: start,
                }),
            )?;
        }
        // Queue data is safe even if processing fails or the process stops after checkpointing.
        loop {
            let (ok, fail) = board.drain_source(actor, name, 500)?;
            report.applied += ok;
            report.failed += fail;
            if ok + fail < 500 {
                break;
            }
        }
        Ok(report)
    }
    pub fn send_next(&self, b: &Board, who: &str, name: &str) -> Result<Option<Job>> {
        let src = b.source(who, name)?;
        ensure!(
            src.outbound,
            "publishing is disabled; enable it explicitly for this source"
        );
        ensure!(
            src.provider == self.provider && self.token.is_some(),
            "publishing requires the source's provider credentials"
        );
        let Some(l) = b.lease_source(who, name, Direction::Out, 120)? else {
            return Ok(None);
        };
        let d: Delivery = serde_json::from_value(l.job.payload.clone())?;
        let find = |id| -> Result<Option<Copy>> {
            Ok(b.source_copies(who, name, id)?
                .into_iter()
                .find(|c| c.thread == l.job.thread))
        };
        let copy = find(d.message)?;
        let parent = d.parent.map(find).transpose()?.flatten();
        // The database records this intent before the HTTP request, including across process death.
        b.start_sending(who, name, l.job.id, &l.receipt)?;
        let result = match self.provider {
            Provider::Slack if d.asset.is_some() => {
                self.publish_file(b, who, &src, &l.job.thread, &d)
            }
            Provider::Github => {
                self.github_send(&src, &l.job.thread, &d, copy.as_ref(), parent.as_ref())
            }
            Provider::Slack => self.slack_send(&src, &l.job.thread, &d, copy.as_ref()),
            Provider::Linear => {
                self.linear_send(&src, &l.job.thread, &d, copy.as_ref(), parent.as_ref())
            }
            Provider::Local => bail!("not a network source"),
        };
        match result {
            Ok(r) => {
                b.finish_source(who, name, l.job.id, &l.receipt, Some(&r))?;
            }
            Err(e) => {
                let status = e.downcast_ref::<HttpStatus>();
                let ambiguous = status.is_none_or(|s| s.0 >= 500 || s.0 == 408);
                b.fail_source(
                    who,
                    name,
                    l.job.id,
                    &l.receipt,
                    FailureReport {
                        reason: &e.to_string().chars().take(450).collect::<String>(),
                        uncertain: ambiguous,
                        retry_after: status.and_then(|s| s.1),
                    },
                )?;
                return Err(e.context(if ambiguous {
                    "uncertain delivery; reconcile the remote copy before retrying"
                } else {
                    "remote rejected delivery; queued for retry"
                }));
            }
        }
        Ok(Some(b.queue_item(who, name, l.job.id)?))
    }
}
