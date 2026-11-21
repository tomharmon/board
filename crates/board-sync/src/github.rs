use super::*;

fn repo(s: &Source) -> Result<String> {
    let p = s.remote.split('/').collect::<Vec<_>>();
    ensure!(
        p.len() == 2
            && p.iter().all(|v| !v.is_empty()
                && *v != "."
                && *v != ".."
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))),
        "expected GitHub OWNER/REPO"
    );
    Ok(format!("/repos/{}/{}", seg(p[0]), seg(p[1])))
}
fn num(s: &str) -> Result<u64> {
    s.parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .context("expected a positive GitHub issue or comment number")
}
fn user(v: &Value) -> String {
    v["user"]["id"]
        .as_u64()
        .map(|id| format!("u{id}"))
        .unwrap_or_else(|| "ghost".into())
}
impl Connector {
    fn gh_pages(&self, path: &str, query: &[(&str, String)]) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        for page in 1..=1000 {
            let mut q = query.to_vec();
            q.push(("per_page", "100".into()));
            q.push(("page", page.to_string()));
            let response = self.req(Method::GET, path, &q, None)?;
            let items = response.as_array().context("expected GitHub list")?;
            out.extend(items.iter().cloned());
            if items.len() < 100 {
                return Ok(out);
            }
        }
        bail!("GitHub page ceiling reached; no checkpoint was advanced")
    }
    pub(super) fn github_threads(&self, s: &Source, since: Option<&str>) -> Result<Vec<String>> {
        let base = repo(s)?;
        let mut q = vec![
            ("state", "all".into()),
            ("sort", "updated".into()),
            ("direction", "asc".into()),
        ];
        if let Some(at) = since {
            q.push(("since", at.into()));
        }
        let mut out = std::collections::BTreeSet::new();
        for item in self.gh_pages(&format!("{base}/issues"), &q)? {
            out.insert(id(&item, "number")?);
        }
        if let Some(at) = since {
            // An edited comment need not bump the issue's own updated_at.
            for (path, field) in [
                ("issues/comments", "issue_url"),
                ("pulls/comments", "pull_request_url"),
            ] {
                for comment in self.gh_pages(&format!("{base}/{path}"), &[("since", at.into())])? {
                    let url = super::s(&comment, field)?;
                    let n = num(url.rsplit('/').next().unwrap_or(""))?;
                    out.insert(n.to_string());
                }
            }
        }
        Ok(out.into_iter().collect())
    }
    pub(super) fn github_read(&self, src: &Source, thread: &str) -> Result<Vec<Envelope>> {
        let n = num(thread)?;
        let t = n.to_string();
        let base = repo(src)?;
        let path = format!("{base}/issues/{n}");
        let issue = self.req(Method::GET, &path, &[], None)?;
        let is_pr = !issue["pull_request"].is_null();
        let pr = if is_pr {
            Some(self.req(Method::GET, &format!("{base}/pulls/{n}"), &[], None)?)
        } else {
            None
        };
        let state = if pr.as_ref().is_some_and(|p| p["merged"] == true) {
            "merged"
        } else if pr.as_ref().is_some_and(|p| p["draft"] == true) && s(&issue, "state")? == "open" {
            "draft"
        } else {
            s(&issue, "state")?
        };
        let mut root = upsert(
            &t,
            "issue".into(),
            user(&issue),
            None,
            content(
                &format!(
                    "{} #{n}: {}",
                    if is_pr { "PR" } else { "Issue" },
                    s(&issue, "title")?
                ),
                &issue["body"],
            ),
        );
        root.url = Some(s(&issue, "html_url")?.into());
        root.updated_at = stamp(&issue, "updated_at")?;
        labels(
            &mut root,
            &[
                (
                    if is_pr { "pr" } else { "issue" },
                    &format!("{}#{n}", src.remote),
                ),
                ("state", state),
            ],
        );
        github_properties(&mut root, &issue, pr.as_ref());
        let mut rest = Vec::new();
        for v in self.gh_pages(&format!("{path}/comments"), &[])? {
            let mut m = upsert(
                &t,
                format!("comment:{}", id(&v, "id")?),
                user(&v),
                Some("issue".into()),
                content("", &v["body"]),
            );
            if matches!(&m.change,Change::Upsert{text,..} if text.is_empty()) {
                continue;
            }
            m.url = Some(s(&v, "html_url")?.into());
            m.updated_at = stamp(&v, "updated_at")?;
            rest.push(envelope(m, v));
        }
        if is_pr {
            let mut latest = None;
            for v in self.gh_pages(&format!("{base}/pulls/{n}/reviews"), &[])? {
                let state = s(&v, "state")?.to_lowercase();
                if state == "pending" {
                    continue;
                }
                let mut m = upsert(
                    &t,
                    format!("review:{}", id(&v, "id")?),
                    user(&v),
                    Some("issue".into()),
                    content(&format!("Review: {state}"), &v["body"]),
                );
                m.url = v["html_url"].as_str().map(str::to_owned);
                m.updated_at = stamp(&v, "submitted_at")?;
                root.updated_at = root.updated_at.max(m.updated_at);
                rest.push(envelope(m, v));
                if matches!(
                    state.as_str(),
                    "approved" | "changes_requested" | "dismissed"
                ) {
                    latest = Some(state);
                }
            }
            if let Some(review) = latest {
                labels(&mut root, &[("review", &review)]);
            }
            for v in self.gh_pages(&format!("{base}/pulls/{n}/comments"), &[])? {
                let mut m = upsert(
                    &t,
                    format!("inline:{}", id(&v, "id")?),
                    user(&v),
                    Some(
                        v["in_reply_to_id"]
                            .as_u64()
                            .map(|p| format!("inline:{p}"))
                            .unwrap_or_else(|| "issue".into()),
                    ),
                    content(
                        &format!(
                            "{}:{}",
                            s(&v, "path")?,
                            v["line"]
                                .as_u64()
                                .or(v["original_line"].as_u64())
                                .map(|n| n.to_string())
                                .unwrap_or_else(|| "file".into())
                        ),
                        &v["body"],
                    ),
                );
                m.url = Some(s(&v, "html_url")?.into());
                m.updated_at = stamp(&v, "updated_at")?;
                rest.push(envelope(m, v));
            }
        }
        rest.sort_by_key(|e| e.import.updated_at);
        let mut out = vec![envelope(root, issue)];
        out.extend(rest);
        Ok(out)
    }
    pub(super) fn github_send(
        &self,
        src: &Source,
        thread: &str,
        d: &Delivery,
        copy: Option<&Copy>,
        parent: Option<&Copy>,
    ) -> Result<DeliveryResult> {
        let base = repo(src)?;
        if d.event == "thread.start" {
            let mut lines = d.text.lines();
            let title = lines
                .next()
                .filter(|x| !x.is_empty())
                .unwrap_or("Board thread");
            let v = self.req(
                Method::POST,
                &format!("{base}/issues"),
                &[],
                Some(&json!({"title":title,"body":lines.collect::<Vec<_>>().join("\n")})),
            )?;
            return Ok(DeliveryResult {
                thread: Some(id(&v, "number")?),
                external: "issue".into(),
                url: v["html_url"].as_str().map(str::to_owned),
            });
        }
        let n = num(thread)?;
        let prefix = parent.and_then(|p| p.external.strip_prefix("inline:"));
        if d.event == "message.posted" {
            let path = match prefix {
                Some(k) => format!("{base}/pulls/{n}/comments/{}/replies", num(k)?),
                None => format!("{base}/issues/{n}/comments"),
            };
            let v = self.req(Method::POST, &path, &[], Some(&json!({"body":d.text})))?;
            return Ok(DeliveryResult {
                thread: None,
                external: format!(
                    "{}:{}",
                    if prefix.is_some() {
                        "inline"
                    } else {
                        "comment"
                    },
                    id(&v, "id")?
                ),
                url: v["html_url"].as_str().map(str::to_owned),
            });
        }
        let c = copy.context("no GitHub copy to change")?;
        if c.external == "issue" {
            ensure!(!c.origin, "only a connector's own roots may be changed");
            let (title, body) = super::root_text(d);
            self.req(
                Method::PATCH,
                &format!("{base}/issues/{n}"),
                &[],
                Some(&json!({"title":title,"body":body})),
            )?;
            return Ok(DeliveryResult {
                thread: None,
                external: c.external.clone(),
                url: c.url.clone(),
            });
        }
        let (kind, i) = c
            .external
            .split_once(':')
            .context("expected a delivered comment")?;
        ensure!(
            !c.origin && matches!(kind, "inline" | "comment"),
            "only a connector's own comments may be changed"
        );
        let path = format!(
            "{base}/{}/comments/{}",
            if kind == "inline" { "pulls" } else { "issues" },
            num(i)?
        );
        if d.event == "message.deleted" {
            self.req(Method::DELETE, &path, &[], None)?;
        } else {
            self.req(Method::PATCH, &path, &[], Some(&json!({"body":d.text})))?;
        }
        Ok(DeliveryResult {
            thread: None,
            external: c.external.clone(),
            url: c.url.clone(),
        })
    }
}
pub(super) fn github_properties(m: &mut Import, i: &Value, pr: Option<&Value>) {
    if let Some(a) = i["labels"].as_array() {
        for tag in a {
            if let Some(k) = tag["name"].as_str() {
                property(m, "tag", k);
            }
        }
    }
    if let Some(a) = i["assignees"].as_array() {
        for user in a {
            if let Some(k) = user["login"].as_str() {
                property(m, "assignee", k);
            }
        }
    }
    if let Some(h) = pr.and_then(|p| p["head"]["sha"].as_str()) {
        labels(m, &[("head", h)]);
    }
}
