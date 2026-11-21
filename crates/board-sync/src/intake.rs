//! Offline intake of explicitly supplied provider event files, not a public webhook server.
use super::*;

/// Enqueue one native delivery for offline processing. A provider ID must be reused on retry.
pub fn ingest(
    board: &Board,
    actor: &str,
    name: &str,
    kind: &str,
    delivery: &str,
    mut raw: Value,
) -> Result<i64> {
    let source = board.source(actor, name)?;
    let obj = raw
        .as_object_mut()
        .context("expected a provider event object")?;
    // Older Slack envelopes include a verification token. It is not data for the inbox.
    for key in ["token", "access_token", "authorization", "signing_secret"] {
        obj.remove(key);
    }
    if source.provider == Provider::Github
        && kind != "workflow_run"
        && source.settings.github_items != GithubItems::All
    {
        ensure!(
            s(&raw["repository"], "full_name")?.eq_ignore_ascii_case(&source.remote),
            "GitHub event is outside configured repository"
        );
        let item = raw
            .get("issue")
            .or(raw.get("pull_request"))
            .context("missing issue or PR")?;
        let t = id(item, "number")?;
        let pr = raw.get("pull_request").is_some() || !item["pull_request"].is_null();
        if !source.settings.github_items.includes(pr)
            && board.find_source(actor, name, &t, None)?.is_none()
        {
            let mut m = upsert(&t, "filtered".into(), "system".into(), None, String::new());
            m.change = Change::Ignore;
            let mut e = envelope(m, Value::Null);
            e.key = format!("event:{delivery}");
            return Ok(board.enqueue_imports(actor, name, &[e], None)?[0]);
        }
    }
    if source.provider == Provider::Github && kind == "workflow_run" {
        ensure!(
            s(&raw["repository"], "full_name")?.eq_ignore_ascii_case(&source.remote),
            "GitHub event is outside configured repository"
        );
        let w = &raw["workflow_run"];
        let run = id(w, "id")?;
        let conclusion = w["conclusion"].as_str().unwrap_or("");
        let failed = matches!(conclusion, "failure" | "timed_out" | "startup_failure");
        if raw["action"] != "completed"
            || (!failed && conclusion != "success")
            || arr(w, "pull_requests")?.is_empty()
        {
            let mut m = upsert(
                &format!("run:{run}"),
                format!("run:{run}"),
                "actions".into(),
                None,
                String::new(),
            );
            m.change = Change::Ignore;
            let mut e = envelope(m, raw);
            e.key = format!("event:{delivery}");
            return Ok(board.enqueue_imports(actor, name, &[e], None)?[0]);
        }
        let head = s(w, "head_sha")?;
        let repo = id(&raw["repository"], "id")?;
        let path = s(w, "path")?
            .split('@')
            .next()
            .unwrap_or("")
            .rsplit('/')
            .next()
            .unwrap_or("");
        let file = property_slug(
            path.strip_suffix(".yml")
                .or_else(|| path.strip_suffix(".yaml"))
                .unwrap_or(path),
        );
        let mut items = Vec::new();
        for pr in arr(w, "pull_requests")? {
            let number = id(pr, "number")?;
            let key: String = format!("ci.{repo}-{number}.{file}")
                .chars()
                .take(64)
                .collect();
            ensure!(
                key.len() <= 64 && !file.is_empty(),
                "invalid workflow label"
            );
            let mut m = upsert(
                &number,
                format!(
                    "run:{run}:{}:{conclusion}",
                    w["run_attempt"].as_u64().unwrap_or(1)
                ),
                "actions".into(),
                None,
                String::new(),
            );
            m.url = w["html_url"].as_str().map(str::to_owned);
            m.change = Change::Signal {
                conditions: BTreeMap::from([("head".into(), head.into())]),
                set: if failed {
                    BTreeMap::from([(key.clone(), conclusion.into())])
                } else {
                    Default::default()
                },
                unset: if failed { Vec::new() } else { vec![key] },
                reply: failed.then(|| {
                    format!(
                        "{}: {conclusion} on {}#{number} at {head}\n{}",
                        s(w, "name").unwrap_or("Workflow"),
                        source.remote,
                        m.url.as_deref().unwrap_or("")
                    )
                }),
            };
            let mut e = envelope(m, raw.clone());
            e.key = format!("event:{delivery}");
            items.push(e);
        }
        ensure!(
            !items.is_empty(),
            "workflow did not identify any pull request"
        );
        return Ok(board.enqueue_imports(actor, name, &items, None)?[0]);
    }
    let import = match source.provider {
        Provider::Github => github(&source, kind, &raw)?,
        Provider::Slack => slack(board, actor, &source, &raw)?,
        Provider::Linear => linear(board, actor, &source, &raw)?,
        Provider::Local => bail!("use inbox push for normalized local events"),
    };
    // GitHub includes the parent issue/PR in a comment delivery. Establish an unknown
    // root first without fetching, and never overwrite a known root with this snapshot.
    let mut batch = Vec::new();
    if source.provider == Provider::Github
        && import.parent.is_some()
        && let Some(item) = raw.get("issue").or(raw.get("pull_request"))
        && item["title"].is_string()
        && item["state"].is_string()
        && board
            .find_source(actor, name, &import.thread, None)?
            .is_none()
    {
        let mut root = github(
            &source,
            if raw.get("pull_request").is_some() {
                "pull_request"
            } else {
                "issues"
            },
            &raw,
        )?;
        if let Change::Reconcile { text, labels, .. } = root.change {
            root.change = Change::Post { text, labels };
        }
        let mut e = envelope(root, Value::Null);
        e.key = format!("anchor:{delivery}");
        batch.push(e);
    }
    // Write the root update and its lifecycle signal into the inbox in order and together.
    let mut secondary = None;
    if source.provider == Provider::Github {
        let action = raw["action"].as_str().unwrap_or("");
        let author = raw["sender"]["login"].as_str().unwrap_or("someone");
        let mut set = BTreeMap::new();
        let mut unset = Vec::new();
        let reply = match (kind, action) {
            ("pull_request" | "issues", "closed" | "reopened") => Some(format!(
                "{} by {author}",
                if raw["pull_request"]["merged"] == true {
                    "merged"
                } else {
                    action
                }
            )),
            ("pull_request_review", "submitted") => {
                if let Some(verdict) = raw["review"]["state"]
                    .as_str()
                    .map(str::to_lowercase)
                    .filter(|s| matches!(s.as_str(), "approved" | "changes_requested"))
                {
                    set.insert("review".into(), verdict);
                }
                None
            }
            ("pull_request_review", "dismissed") => {
                unset.push("review".into());
                None
            }
            _ => None,
        };
        if reply.is_some() || !set.is_empty() || !unset.is_empty() {
            let mut m = upsert(
                &import.thread,
                format!("activity:{delivery}"),
                actor_id(&raw["sender"]),
                None,
                String::new(),
            );
            m.change = Change::Signal {
                conditions: BTreeMap::new(),
                set,
                unset,
                reply,
            };
            m.url = import.url.clone();
            let mut x = envelope(m, Value::Null);
            x.key = format!("activity:{delivery}");
            secondary = Some(x);
        }
    }
    let mut e = envelope(import, raw);
    e.key = format!("event:{delivery}");
    let ordinal = batch.len();
    batch.push(e);
    batch.extend(secondary);
    Ok(board.enqueue_imports(actor, name, &batch, None)?[ordinal])
}
fn github(src: &Source, kind: &str, v: &Value) -> Result<Import> {
    ensure!(
        s(&v["repository"], "full_name")?.eq_ignore_ascii_case(&src.remote),
        "GitHub event is outside configured repository"
    );
    let action = s(v, "action")?;
    let owner = v
        .get("issue")
        .or(v.get("pull_request"))
        .context("missing issue or PR")?;
    let t = id(owner, "number")?;
    let is_pr = v.get("pull_request").is_some() || owner.get("pull_request").is_some();
    let mut m = match kind {
        "issues" | "pull_request" => {
            let title = s(owner, "title")?;
            let mut m = upsert(
                &t,
                "issue".into(),
                actor_id(&owner["user"]),
                None,
                content(
                    &format!("{} #{t}: {title}", if is_pr { "PR" } else { "Issue" }),
                    &owner["body"],
                ),
            );
            let state = if owner["merged"] == true {
                "merged"
            } else if owner["draft"] == true && owner["state"] == "open" {
                "draft"
            } else {
                s(owner, "state")?
            };
            labels(
                &mut m,
                &[
                    (
                        if is_pr { "pr" } else { "issue" },
                        &format!("{}#{t}", src.remote),
                    ),
                    ("state", state),
                ],
            );
            super::github::github_properties(&mut m, owner, is_pr.then_some(owner));
            if let Change::Upsert { text, labels } = &m.change {
                let mut owns = Vec::new();
                for (field, prefix) in [("labels", "tag."), ("assignees", "assignee.")] {
                    if owner[field].is_array() {
                        owns.push(prefix.into());
                    }
                }
                let mut reset = BTreeMap::new();
                if is_pr && let Ok(repo) = id(&v["repository"], "id") {
                    reset.insert("head".into(), format!("ci.{repo}-{t}."));
                }
                m.change = Change::Reconcile {
                    text: text.clone(),
                    labels: labels.clone(),
                    owns,
                    reset,
                    existing_only: is_pr && action == "synchronize",
                };
            }
            m.url = owner["html_url"].as_str().map(str::to_string);
            m.updated_at = stamp(owner, "updated_at")?;
            m
        }
        "issue_comment" | "pull_request_review_comment" | "pull_request_review" => {
            let review = kind == "pull_request_review";
            let inline = kind == "pull_request_review_comment";
            let c = &v[if review { "review" } else { "comment" }];
            let prefix = if review {
                "review"
            } else if inline {
                "inline"
            } else {
                "comment"
            };
            let info = if review {
                format!("Review: {}", s(c, "state")?.to_lowercase())
            } else if inline {
                format!(
                    "{}:{}",
                    s(c, "path")?,
                    c["line"]
                        .as_u64()
                        .or(c["original_line"].as_u64())
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "file".into())
                )
            } else {
                String::new()
            };
            let mut m = upsert(
                &t,
                format!("{prefix}:{}", id(c, "id")?),
                actor_id(&c["user"]),
                Some(
                    c["in_reply_to_id"]
                        .as_u64()
                        .map(|n| format!("inline:{n}"))
                        .unwrap_or_else(|| "issue".into()),
                ),
                content(&info, &c["body"]),
            );
            m.url = c["html_url"].as_str().map(str::to_string);
            m.updated_at = stamp(c, if review { "submitted_at" } else { "updated_at" })?;
            m
        }
        _ => bail!("unsupported GitHub event kind"),
    };
    if action == "deleted" {
        m.change = Change::Delete;
        m.updated_at = None;
    }
    Ok(m)
}
fn actor_id(v: &Value) -> String {
    v["id"]
        .as_u64()
        .map(|n| format!("u{n}"))
        .unwrap_or_else(|| "ghost".into())
}
fn slack(b: &Board, who: &str, src: &Source, v: &Value) -> Result<Import> {
    let e = &v["event"];
    let typ = s(e, "type")?;
    let reaction = matches!(typ, "reaction_added" | "reaction_removed");
    ensure!(reaction || typ == "message", "unsupported Slack event kind");
    let target = if reaction { &e["item"] } else { e };
    ensure!(
        target["channel"] == src.remote,
        "Slack event is outside configured channel"
    );
    if reaction {
        ensure!(
            target["type"] == "message",
            "only Slack message reactions are supported"
        );
    }
    let changed = e["subtype"] == "message_changed";
    let deleted = e["subtype"] == "message_deleted";
    let msg = if reaction {
        target
    } else if changed {
        &e["message"]
    } else if deleted {
        &e["previous_message"]
    } else {
        e
    };
    let external = if deleted {
        s(e, "deleted_ts")?
    } else {
        s(msg, "ts")?
    };
    super::slack::date(external)?;
    let thread = match msg.get("thread_ts").and_then(Value::as_str) {
        Some(t) => t.to_owned(),
        None => b
            .source_thread_for(who, &src.name, external)?
            .unwrap_or_else(|| external.into()),
    };
    let a = if reaction {
        e["user"].as_str()
    } else {
        msg["user"].as_str().or(msg["bot_id"].as_str())
    }
    .unwrap_or("system");
    let hints = super::slack::unfurls(msg);
    let original = super::slack::with_forward(msg["text"].as_str().unwrap_or(""), msg, &hints);
    let who_text = Some(original.as_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("[message without text]");
    let (body, names) = slack_text::incoming(&src.name, who_text);
    let mut m = upsert(
        &thread,
        external.into(),
        a.into(),
        (thread != external).then(|| thread.clone()),
        body,
    );
    m.mentioned = names;
    m.links = hints;
    m.updated_at = Some(super::slack::date(
        e["event_ts"].as_str().unwrap_or(external),
    )?);
    m.url = Some(format!(
        "https://app.slack.com/archives/{}/p{}",
        seg(&src.remote),
        external.replace('.', "")
    ));
    if deleted {
        m.change = Change::Delete;
    } else if reaction {
        m.change = Change::React {
            emoji: s(e, "reaction")?.into(),
            remove: typ == "reaction_removed",
        };
    }
    Ok(m)
}
fn linear(b: &Board, who: &str, src: &Source, v: &Value) -> Result<Import> {
    let typ = s(v, "type")?;
    let d = &v["data"];
    let action = s(v, "action")?;
    ensure!(
        matches!(action, "create" | "update" | "remove"),
        "unsupported Linear action"
    );
    let mut m = match typ {
        "Issue" => {
            let id = s(d, "id")?;
            let identifier = s(d, "identifier")?;
            let key = d["team"]["key"]
                .as_str()
                .or_else(|| identifier.rsplit_once('-').map(|(key, _)| key));
            ensure!(
                key == Some(&src.remote),
                "Linear issue is outside configured team"
            );
            let a = d["creatorId"]
                .as_str()
                .or(d["creator"]["id"].as_str())
                .unwrap_or("system");
            let mut m = upsert(
                id,
                "issue".into(),
                a.into(),
                None,
                content(
                    &format!(
                        "{identifier}: {}",
                        d["title"].as_str().unwrap_or("[removed]")
                    ),
                    &d["description"],
                ),
            );
            labels(&mut m, &[("issue", identifier)]);
            super::linear::linear_properties(&mut m, d);
            if let Change::Reconcile { existing_only, .. } = &mut m.change {
                *existing_only = action == "update"
                    && (v["actor"]["type"] != "user" || !d["archivedAt"].is_null());
            }
            m
        }
        "Comment" => {
            let issue = d["issueId"]
                .as_str()
                .or(d["issue"]["id"].as_str())
                .context("missing Linear parent issue")?;
            ensure!(
                b.find_source(who, &src.name, issue, None)?.is_some(),
                "import the issue in this team before its comments"
            );
            let a = d["userId"]
                .as_str()
                .or(d["user"]["id"].as_str())
                .unwrap_or("system");
            upsert(
                issue,
                format!("comment:{}", s(d, "id")?),
                a.into(),
                Some(
                    d["parentId"]
                        .as_str()
                        .map(|s| format!("comment:{s}"))
                        .unwrap_or_else(|| "issue".into()),
                ),
                d["body"].as_str().unwrap_or("").into(),
            )
        }
        _ => bail!("unsupported Linear event kind"),
    };
    m.updated_at = stamp(d, "updatedAt")?;
    m.url = d["url"].as_str().or(v["url"].as_str()).map(str::to_string);
    if action == "remove" {
        m.change = Change::Delete;
        m.updated_at = None;
    }
    Ok(m)
}
