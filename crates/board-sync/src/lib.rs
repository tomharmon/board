//! Explicitly-invoked clients for three optional providers. Board's worker and HTTP server never call these clients.
mod github;
mod intake;
mod linear;
mod media;
mod signed;
mod slack;
mod slack_text;
pub use intake::ingest;
pub use signed::verify_intake;
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
    pub skipped: usize,
    pub failed: usize,
    pub files: usize,
    pub files_deferred: usize,
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
fn utf16_prefix(s: &str, mut units: usize) -> &str {
    for (i, c) in s.char_indices() {
        if c.len_utf16() > units {
            return &s[..i];
        }
        units -= c.len_utf16();
    }
    s
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
    if let Change::Post { text, .. }
    | Change::Upsert { text, .. }
    | Change::Reconcile { text, .. }
    | Change::Edit { text } = &mut import.change
        && text.encode_utf16().count() > 10_000
    {
        let marker = "\n[truncated; original in connector inbox]";
        text.truncate(utf16_prefix(text, 10_000 - marker.len()).len());
        text.push_str(marker);
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
        links: Vec::new(),
        no_sync: false,
        mentioned: Default::default(),
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
    let slug = property_slug(name);
    if !slug.is_empty() {
        let key = format!(
            "{prefix}.{}",
            slug.chars().take(63 - prefix.len()).collect::<String>()
        );
        let value: String = name.chars().filter(|c| !c.is_control()).collect();
        labels(m, &[(&key, utf16_prefix(&value, 256))]);
    }
}
fn property_slug(name: &str) -> String {
    let normalizer = icu_normalizer::DecomposingNormalizerBorrowed::new_nfkd();
    let mut result = String::new();
    let mut separator = false;
    for c in normalizer
        .normalize_iter(name.chars())
        .filter(|c| !('\u{300}'..='\u{36f}').contains(c))
        .flat_map(char::to_lowercase)
    {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
            if separator {
                result.push('-');
                separator = false;
            }
            result.push(c);
        } else {
            separator = true;
        }
    }
    result
        .trim_start_matches(['-', '_', '.'])
        .trim_end_matches('-')
        .to_owned()
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
/// Parse a provider link for its identifier only. Never use this URL as a request destination.
fn provider_url(raw: &str, host: &str) -> Result<reqwest::Url> {
    let u = reqwest::Url::parse(raw)?;
    ensure!(
        u.scheme() == "https"
            && u.host_str() == Some(host)
            && u.port_or_known_default() == Some(443)
            && u.username().is_empty()
            && u.password().is_none(),
        "expected an HTTPS link on {host}"
    );
    Ok(u)
}
impl Connector {
    /// Preview an explicitly configured source; neither a read nor a channel join creates Board records.
    pub fn preview(&self, b: &Board, who: &str, name: &str, thread: Option<&str>) -> Result<Value> {
        self.preview_target(b, who, name, thread, None, None, 15)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn preview_target(
        &self,
        b: &Board,
        who: &str,
        name: &str,
        thread: Option<&str>,
        channel: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Value> {
        b.require_owner()?;
        let mut src = b.source(who, name)?;
        ensure!(
            src.provider == self.provider
                && (1..=100).contains(&limit)
                && cursor.is_none_or(|s| s.len() <= 1024 && !s.chars().any(char::is_control)),
            "invalid preview options"
        );
        if self.provider == Provider::Slack {
            let (ch, at) = match thread {
                Some(raw)
                    if let Some((ch, ts, parent)) = board_local::links::slack_address(raw) =>
                {
                    (Some(ch), Some(parent.unwrap_or(ts)))
                }
                Some(raw) if let Some((ch, ts)) = raw.split_once(':') => {
                    slack::ts(ts)?;
                    (Some(ch.to_owned()), Some(ts.to_owned()))
                }
                Some(raw) if raw.starts_with('#') || raw.starts_with('C') => {
                    (Some(raw.to_owned()), None)
                }
                Some(raw) => {
                    slack::ts(raw)?;
                    (None, Some(raw.to_owned()))
                }
                None => (None, None),
            };
            ensure!(
                ch.is_none() || channel.is_none(),
                "specify the channel only once"
            );
            let info =
                self.public_slack_channel(ch.as_deref().or(channel).unwrap_or(&src.remote))?;
            ensure!(
                info["is_member"] == true,
                "the Slack app is not a member of this public channel"
            );
            src.remote = id(&info, "id")?;
            return self.slack_preview_page(&src, at.as_deref(), cursor, limit);
        }
        ensure!(
            channel.is_none(),
            "--channel applies only to Slack previews"
        );
        if self.provider == Provider::Linear {
            return self.linear_preview(&src, thread, cursor, limit);
        }
        ensure!(
            thread.is_none() || cursor.is_none(),
            "a provider thread is read as a whole; a cursor pages the listing"
        );
        match (&self.provider, thread) {
            (Provider::Github, Some(t)) => {
                let n = github::issue_number(&src, t)?;
                Ok(
                    json!({"records":order(self.github_read(&src,&n.to_string(),true)?)?.into_iter().map(|v|v.import).collect::<Vec<_>>()}),
                )
            }
            (Provider::Github, None) => self.github_preview_page(&src, cursor, limit),
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
                Provider::Github => self.github_read(
                    &source,
                    &t,
                    board.find_source(actor, name, &t, None)?.is_some(),
                )?,
                Provider::Slack => self.slack_read(&source, &t)?,
                Provider::Linear => self.linear_read(&source, &t)?,
                Provider::Local => unreachable!(),
            };
            if events.is_empty() {
                continue;
            }
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
            let batch = board.drain_source_report(actor, name, 500)?;
            report.applied += batch.applied;
            report.failed += batch.failed;
            report.skipped += batch.skipped;
            if batch.processed() < 500 {
                break;
            }
        }
        if source.provider == Provider::Slack && source.settings.mirror_files {
            for item in board.pending_media(actor, name, false)? {
                match self.fetch_file_limit(
                    board,
                    actor,
                    name,
                    &item.thread,
                    &item.external,
                    &item.file,
                    25 * 1024 * 1024,
                ) {
                    Ok(_) => report.files += 1,
                    Err(e) => {
                        board.record_media_failure(actor, name, &item, &e.to_string())?;
                        report.files_deferred += 1;
                    }
                }
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
        let destination = |copy: Option<&Copy>| -> Result<String> {
            if self.provider != Provider::Linear || l.job.payload["event"] == "thread.start" {
                return Ok(l.job.thread.clone());
            }
            let comment = copy.and_then(|c| c.external.strip_prefix("comment:"));
            match self.linear_destination(&src, &l.job.thread, comment) {
                Ok(id) => Ok(id),
                Err(e) => {
                    // A read failure before sending can safely retry; no external mutation was attempted.
                    b.fail_source(
                        who,
                        name,
                        l.job.id,
                        &l.receipt,
                        FailureReport {
                            reason: &e.to_string().chars().take(450).collect::<String>(),
                            uncertain: false,
                            retry_after: e.downcast_ref::<HttpStatus>().and_then(|s| s.1),
                        },
                    )?;
                    Err(e.context("Linear destination not verified; nothing sent"))
                }
            }
        };
        if l.job.payload["event"] == "source.response" {
            let text = s(&l.job.payload, "text")?;
            let target = destination(None)?;
            b.start_sending(who, name, l.job.id, &l.receipt)?;
            return self.confirm(b, who, name, &l, self.respond(&src, &target, text));
        }
        let mut d: Delivery = serde_json::from_value(l.job.payload.clone())?;
        let find = |id| -> Result<Option<Copy>> {
            Ok(b.source_copies(who, name, id)?
                .into_iter()
                .find(|c| c.thread == l.job.thread))
        };
        let copy = find(d.message)?;
        let parent = d.parent.map(find).transpose()?.flatten();
        if matches!(
            d.event.as_str(),
            "message.posted" | "message.edited" | "thread.start" | "file.note"
        ) {
            let m = b.get(d.message)?;
            if src.settings.display_authors {
                let who = m
                    .author
                    .as_ref()
                    .map_or(m.actor.as_str(), |a| a.name.as_str());
                let name: String = who
                    .chars()
                    .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-' | '/'))
                    .take(80)
                    .collect();
                d.text.push_str(&format!("\n\nFrom {name} on Board"));
            }
            let declined = if self.provider == Provider::Slack {
                b.declined_assets(who, name, &l.job.thread, m.id)?
            } else {
                Vec::new()
            };
            for file in &m.attachments {
                if self.provider != Provider::Slack || declined.contains(&file.id) {
                    let filename = file.name.replace('`', "'");
                    d.text.push_str(&format!(
                        "\n\nFile: `{filename}` ({} bytes; on Board #{})",
                        file.size, m.id
                    ));
                }
            }
        }
        let target = destination(if d.event == "message.posted" {
            parent.as_ref()
        } else {
            copy.as_ref()
        })?;
        // The database records this intent before the mutating request, including across process death.
        b.start_sending(who, name, l.job.id, &l.receipt)?;
        let result = match self.provider {
            Provider::Slack if d.asset.is_some() => {
                self.publish_file(b, who, &src, &l.job.thread, &d)
            }
            Provider::Github => {
                self.github_send(&src, &l.job.thread, &d, copy.as_ref(), parent.as_ref())
            }
            Provider::Slack => self.slack_send(&src, &l.job.thread, &d, copy.as_ref()),
            Provider::Linear => self.linear_send(&src, &target, &d, copy.as_ref(), parent.as_ref()),
            Provider::Local => bail!("not a network source"),
        };
        self.confirm(b, who, name, &l, result)
    }
    fn respond(&self, src: &Source, thread: &str, text: &str) -> Result<DeliveryResult> {
        let (external, url) = match self.provider {
            Provider::Github => {
                let n = thread.parse::<u64>()?;
                let v = self.req(
                    Method::POST,
                    &format!("{}/issues/{n}/comments", github::repo(src)?),
                    &[],
                    Some(&json!({"body":text})),
                )?;
                (
                    format!("comment:{}", id(&v, "id")?),
                    v["html_url"].as_str().map(str::to_owned),
                )
            }
            Provider::Slack => {
                slack::ts(thread)?;
                let v=self.sl(Method::POST,"chat.postMessage",&[],Some(&json!({"channel":src.remote,"thread_ts":thread,"text":slack_text::outgoing(&src.name,text),"unfurl_links":false,"unfurl_media":false})))?;
                (id(&v, "ts")?, None)
            }
            Provider::Linear => {
                let v=self.gql("mutation BoardResponse($input:CommentCreateInput!){commentCreate(input:$input){success comment{id url}}}",json!({"input":{"issueId":thread,"body":text}}))?;
                ensure!(
                    v["commentCreate"]["success"] == true,
                    "Linear did not confirm the response"
                );
                let c = &v["commentCreate"]["comment"];
                (
                    format!("comment:{}", id(c, "id")?),
                    c["url"].as_str().map(str::to_owned),
                )
            }
            Provider::Local => bail!("not a network source"),
        };
        Ok(DeliveryResult {
            external,
            url,
            thread: None,
        })
    }
    fn confirm(
        &self,
        b: &Board,
        who: &str,
        name: &str,
        l: &Lease,
        result: Result<DeliveryResult>,
    ) -> Result<Option<Job>> {
        match result {
            Ok(r) => {
                b.ack_source(who, name, l.job.id, &l.receipt, Some(&r))?;
            }
            Err(e) => {
                if e.is::<media::FileRefused>() {
                    b.refused_file(who, name, l.job.id, &l.receipt)?;
                    return Ok(Some(b.queue_item(who, name, l.job.id)?));
                }
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
