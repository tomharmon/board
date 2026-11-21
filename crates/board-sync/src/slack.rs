use super::*;
use board_local::links::{Link, prose_links, slack_address};

/// An unfurl was supplied with this message, not fetched using broader Board permissions.
pub(super) fn unfurls(v: &Value) -> Vec<Link> {
    let mut out = Vec::new();
    for a in v["attachments"].as_array().into_iter().flatten().take(12) {
        let Some(url) = a["original_url"]
            .as_str()
            .or(a["from_url"].as_str())
            .or(a["title_link"].as_str())
            .filter(|u| slack_address(u).is_some())
        else {
            continue;
        };
        let Some(text) = a["text"]
            .as_str()
            .or(a["fallback"].as_str())
            .filter(|t| !t.is_empty())
        else {
            continue;
        };
        out.push(Link {
            url: url.into(),
            message: None,
            author: a["author_name"].as_str().map(str::to_owned),
            text: Some(text.chars().take(2000).collect()),
            reason: None,
        });
    }
    out
}
pub(super) fn with_forward(body: &str, v: &Value, hints: &[Link]) -> String {
    if body.trim().is_empty()
        && v["attachments"]
            .as_array()
            .is_some_and(|a| a.iter().any(|x| x["is_share"] == true))
        && let Some(l) = hints.first()
    {
        return format!(
            "> {}: {}\n{}",
            l.author.as_deref().unwrap_or("Shared message"),
            l.text.as_deref().unwrap_or(""),
            l.url
        );
    }
    body.into()
}
pub(super) fn ts(s: &str) -> Result<()> {
    board_local::sources::validate_slack_timestamp(s)
}
pub(super) fn date(t: &str) -> Result<DateTime<Utc>> {
    ts(t)?;
    let (a, b) = t.split_once('.').unwrap();
    DateTime::from_timestamp(a.parse()?, format!("{b:0<9}").parse()?).context("invalid Slack time")
}
impl Connector {
    pub fn join_public_slack(&self, b: &Board, who: &str, name: &str) -> Result<()> {
        self.join_slack_target(b, who, name, None)
    }
    pub fn join_slack_target(
        &self,
        b: &Board,
        who: &str,
        name: &str,
        channel: Option<&str>,
    ) -> Result<()> {
        b.require_owner()?;
        let src = b.source(who, name)?;
        ensure!(
            self.provider == Provider::Slack && src.provider == Provider::Slack,
            "only Slack supports a public channel join"
        );
        let c = self.public_slack_channel(channel.unwrap_or(&src.remote))?;
        if c["is_member"] != true {
            self.sl(
                Method::POST,
                "conversations.join",
                &[],
                Some(&json!({"channel":c["id"]})),
            )?;
        }
        Ok(())
    }
    pub(super) fn public_slack_channel(&self, target: &str) -> Result<Value> {
        let resolved;
        let target = if let Some((channel, _, _)) = slack_address(target) {
            resolved = channel;
            resolved.as_str()
        } else if let Some(name) = target.strip_prefix('#') {
            resolved = self.slack_channel_named(name)?;
            resolved.as_str()
        } else {
            target
        };
        ensure!(
            target.len() <= 64
                && target.starts_with('C')
                && target.bytes().all(|b| b.is_ascii_alphanumeric()),
            "expected a public channel ID"
        );
        let v = self.sl(
            Method::GET,
            "conversations.info",
            &[("channel", target.to_owned())],
            None,
        )?;
        let c = &v["channel"];
        ensure!(
            c["id"] == target
                && c["is_channel"] == true
                && c["is_private"] == false
                && c["is_shared"] == false
                && c["is_ext_shared"] != true
                && c["is_org_shared"] != true,
            "only a verified public, non-shared Slack channel is allowed"
        );
        Ok(c.clone())
    }
    fn slack_channel_named(&self, name: &str) -> Result<String> {
        ensure!(
            !name.is_empty()
                && name.len() <= 80
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "expected a Slack channel name"
        );
        let mut cursor = String::new();
        let mut seen = HashSet::new();
        for _ in 0..100 {
            let mut q = vec![
                ("types", "public_channel".to_owned()),
                ("limit", "200".to_owned()),
                ("exclude_archived", "true".to_owned()),
            ];
            if !cursor.is_empty() {
                q.push(("cursor", cursor));
            }
            let v = self.sl(Method::GET, "conversations.list", &q, None)?;
            if let Some(c) = arr(&v, "channels")?.iter().find(|c| c["name"] == name) {
                return id(c, "id");
            }
            let next = v["response_metadata"]["next_cursor"].as_str().unwrap_or("");
            ensure!(!next.is_empty(), "no such public Slack channel");
            ensure!(seen.insert(next.to_owned()), "Slack repeated a cursor");
            cursor = next.to_owned();
        }
        bail!("Slack channel listing exceeded the page limit")
    }
    pub(super) fn slack_preview_page(
        &self,
        src: &Source,
        thread: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Value> {
        let mut q = vec![
            ("channel", src.remote.clone()),
            ("limit", limit.min(15).to_string()),
        ];
        if let Some(c) = cursor {
            q.push(("cursor", c.to_owned()));
        }
        let mut root = thread.map(str::to_owned);
        let mut v = if let Some(at) = thread {
            q.push(("ts", at.into()));
            self.sl(Method::GET, "conversations.replies", &q, None)?
        } else {
            self.sl(Method::GET, "conversations.history", &q, None)?
        };
        if let Some(at) = thread
            && cursor.is_none()
            && let Some(parent) = v
                .pointer("/messages/0/thread_ts")
                .and_then(Value::as_str)
                .filter(|p| *p != at)
        {
            ts(parent)?;
            root = Some(parent.to_owned());
            q.last_mut().unwrap().1 = parent.to_owned();
            v = self.sl(Method::GET, "conversations.replies", &q, None)?;
        }
        let next = v["response_metadata"]["next_cursor"]
            .as_str()
            .filter(|n| !n.is_empty());
        ensure!(
            v["has_more"] != true || next.is_some(),
            "Slack did not return a continuation"
        );
        let mut rows = arr(&v, "messages")?.to_vec();
        if root.is_none() {
            rows.reverse();
        }
        let threads = rows
            .iter()
            .map(|v| id(v, "ts"))
            .collect::<Result<Vec<_>>>()?;
        let records = self
            .slack_records(src, root.as_deref(), rows)?
            .into_iter()
            .map(|e| e.import)
            .collect::<Vec<_>>();
        Ok(
            json!({"channel":src.remote,"thread":root,"threads":threads,"records":records,"next":next}),
        )
    }
    pub(super) fn sl(
        &self,
        method: Method,
        route: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Value> {
        let v = self.req(method, &format!("/{route}"), query, body)?;
        if v["ok"] != true {
            let error = v["error"]
                .as_str()
                .unwrap_or("unknown")
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                .take(64)
                .collect::<String>();
            bail!("Slack declined request: {error}");
        }
        Ok(v)
    }
    fn sl_pages(&self, route: &str, params: &[(&str, String)]) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut cursor = String::new();
        for _ in 0..1000 {
            let mut q = params.to_vec();
            q.push(("limit", "15".into()));
            if !cursor.is_empty() {
                q.push(("cursor", cursor.clone()));
            }
            let v = self.sl(Method::GET, route, &q, None)?;
            out.extend(arr(&v, "messages")?.iter().cloned());
            let next = v["response_metadata"]["next_cursor"].as_str().unwrap_or("");
            if next.is_empty() {
                ensure!(
                    v["has_more"] != true,
                    "Slack did not provide a continuation cursor"
                );
                return Ok(out);
            }
            ensure!(seen.insert(next.to_owned()), "Slack repeated a cursor");
            cursor = next.to_owned();
        }
        bail!("Slack page ceiling reached; no checkpoint was advanced")
    }
    pub(super) fn slack_threads(&self, src: &Source) -> Result<Vec<String>> {
        // Scan root history so new replies to an old conversation are never skipped.
        let mut out = Vec::new();
        for m in self.sl_pages("conversations.history", &[("channel", src.remote.clone())])? {
            if m["type"] == "message" {
                out.push(s(&m, "ts")?.into());
            }
        }
        out.reverse();
        Ok(out)
    }
    pub(super) fn slack_read(&self, src: &Source, t: &str) -> Result<Vec<Envelope>> {
        ts(t)?;
        let rows = self.sl_pages(
            "conversations.replies",
            &[("channel", src.remote.clone()), ("ts", t.into())],
        )?;
        ensure!(
            rows.iter().any(|v| v["ts"] == t),
            "Slack thread root was not returned"
        );
        self.slack_records(src, Some(t), rows)
    }
    fn slack_records(
        &self,
        src: &Source,
        thread: Option<&str>,
        rows: Vec<Value>,
    ) -> Result<Vec<Envelope>> {
        let mut out = Vec::new();
        let mut profiles: BTreeMap<String, Option<AuthorInfo>> = BTreeMap::new();
        let mut looked = 0;
        for v in rows {
            let id = s(&v, "ts")?;
            let t = thread.or_else(|| v["thread_ts"].as_str()).unwrap_or(id);
            ts(id)?;
            let actor = v["user"]
                .as_str()
                .or(v["bot_id"].as_str())
                .unwrap_or("system")
                .to_owned();
            let mut hints = unfurls(&v);
            let original = with_forward(v["text"].as_str().unwrap_or(""), &v, &hints);
            let text = Some(original.as_str())
                .filter(|x| !x.trim().is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    v["files"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|f| f["name"].as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .filter(|x| !x.is_empty())
                        .unwrap_or_else(|| "[message without text]".into())
                });
            let (text, mentioned) = slack_text::incoming(&src.name, &text);
            for url in prose_links(&text) {
                if looked >= 8 || hints.iter().any(|h| h.url == url) {
                    continue;
                }
                if let Some((channel, ts, thread)) = slack_address(&url) {
                    looked += 1;
                    hints.push(self.link_hint(&url, &channel, &ts, thread.as_deref()));
                }
            }
            let mut m = upsert(t, id.into(), actor, (id != t).then(|| t.into()), text);
            m.links = hints;
            m.mentioned = mentioned;
            if src.settings.display_authors {
                for uid in std::iter::once(&m.author).chain(m.mentioned.keys()) {
                    if !profiles.contains_key(uid) && profiles.len() < 128 {
                        let p = self
                            .sl(Method::GET, "users.info", &[("user", uid.clone())], None)
                            .ok()
                            .and_then(|v| {
                                if v["user"]["id"] != **uid {
                                    return None;
                                }
                                let p = &v["user"]["profile"];
                                let name = p["display_name"]
                                    .as_str()
                                    .filter(|v| !v.is_empty())
                                    .or(p["real_name"].as_str())?;
                                Some(AuthorInfo {
                                    name: name.to_owned(),
                                    avatar: p["image_72"]
                                        .as_str()
                                        .filter(|u| u.starts_with("https://"))
                                        .map(str::to_owned),
                                })
                            });
                        profiles.insert(uid.clone(), p);
                    }
                }
                m.profile = profiles.get(&m.author).cloned().flatten();
                for (uid, p) in &mut m.mentioned {
                    if let Some(Some(found)) = profiles.get(uid) {
                        *p = found.clone();
                    }
                }
            }
            m.url = Some(format!(
                "https://app.slack.com/archives/{}/p{}",
                seg(&src.remote),
                id.replace('.', "")
            ));
            m.updated_at = Some(date(v["edited"]["ts"].as_str().unwrap_or(id))?);
            if v["subtype"] == "tombstone" {
                m.change = Change::Delete;
            } else if let Some(reactions) = v["reactions"].as_array() {
                for r in reactions {
                    for user in r["users"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                    {
                        let mut reaction = m.clone();
                        reaction.author = user.into();
                        reaction.profile = None;
                        reaction.change = Change::React {
                            emoji: s(r, "name")?.into(),
                            remove: false,
                        };
                        out.push(envelope(
                            reaction,
                            json!({"slack_reaction":r["name"],"actor":user}),
                        ));
                    }
                }
            }
            out.push(envelope(m, v));
        }
        Ok(out)
    }
    fn link_hint(&self, url: &str, channel: &str, at: &str, thread: Option<&str>) -> Link {
        let mut link = Link {
            url: url.into(),
            message: None,
            author: None,
            text: None,
            reason: Some("unavailable".into()),
        };
        let Ok(info) = self.public_slack_channel(channel) else {
            link.reason = Some("not_public".into());
            return link;
        };
        if info["is_member"] != true {
            link.reason = Some("not_in_channel".into());
            return link;
        }
        let query = [
            ("channel", channel.into()),
            ("ts", thread.unwrap_or(at).into()),
            ("oldest", at.into()),
            ("latest", at.into()),
            ("inclusive", "true".into()),
            ("limit", "2".into()),
        ];
        if let Ok(v) = self.sl(Method::GET, "conversations.replies", &query, None)
            && let Some(m) = v["messages"]
                .as_array()
                .and_then(|v| v.iter().find(|m| m["ts"] == at))
            && let Some(t) = m["text"].as_str()
        {
            link.text = Some(t.chars().take(2000).collect());
            link.author = m["user"].as_str().map(str::to_owned);
            link.reason = None;
        }
        link
    }
    pub(super) fn slack_send(
        &self,
        src: &Source,
        t: &str,
        d: &Delivery,
        copy: Option<&Copy>,
    ) -> Result<DeliveryResult> {
        let text = slack_text::outgoing(&src.name, &d.text);
        if d.event == "thread.start" {
            let v=self.sl(Method::POST,"chat.postMessage",&[],Some(&json!({"channel":src.remote,"text":text,"unfurl_links":false,"unfurl_media":false})))?;
            let external = s(&v, "ts")?.to_owned();
            return Ok(DeliveryResult {
                thread: Some(external.clone()),
                external,
                url: None,
            });
        }
        ts(t)?;
        if let Some(emoji) = &d.emoji {
            let c = copy.context("no Slack copy to react to")?;
            ts(&c.external)?;
            let adding = d.event == "reaction.added";
            let route = if adding {
                "/reactions.add"
            } else {
                "/reactions.remove"
            };
            let name = board_local::reactions::shortcode(emoji);
            let v = self.req(
                Method::POST,
                route,
                &[],
                Some(&json!({"channel":src.remote,"timestamp":c.external,"name":name})),
            )?;
            ensure!(
                v["ok"] == true
                    || v["error"]
                        == if adding {
                            "already_reacted"
                        } else {
                            "no_reaction"
                        },
                "Slack did not confirm the reaction"
            );
            return Ok(DeliveryResult {
                thread: None,
                external: c.external.clone(),
                url: c.url.clone(),
            });
        }
        let ext = if d.event == "message.posted" {
            let v=self.sl(Method::POST,"chat.postMessage",&[],Some(&json!({"channel":src.remote,"thread_ts":t,"text":text,"unfurl_links":false,"unfurl_media":false})))?;
            s(&v, "ts")?.into()
        } else {
            let c = copy.context("no Slack copy to change")?;
            ensure!(!c.origin, "only a connector's own messages may be changed");
            ts(&c.external)?;
            let route = if d.event == "message.deleted" {
                "chat.delete"
            } else {
                "chat.update"
            };
            let mut body = json!({"channel":src.remote,"ts":c.external});
            if route == "chat.update" {
                body["text"] = json!(text);
            }
            self.sl(Method::POST, route, &[], Some(&body))?;
            c.external.clone()
        };
        Ok(DeliveryResult {
            thread: None,
            external: ext,
            url: None,
        })
    }
    /// Explicitly publish or clear a transient assistant status, never a background request.
    pub fn slack_status(
        &self,
        b: &Board,
        who: &str,
        name: &str,
        thread: &str,
        status: &str,
    ) -> Result<()> {
        let src = b.source(who, name)?;
        ensure!(
            src.outbound && src.provider == Provider::Slack && self.provider == Provider::Slack,
            "status requires a publishing-enabled Slack source"
        );
        ts(thread)?;
        ensure!(
            status.chars().count() <= 200 && !status.chars().any(char::is_control),
            "invalid status text"
        );
        ensure!(
            b.find_source(who, name, thread, None)?
                .is_some_and(|m| !m.no_sync && !m.deleted),
            "thread is not linked or is local-only"
        );
        self.sl(
            Method::POST,
            "assistant.threads.setStatus",
            &[],
            Some(&json!({"channel_id":src.remote,"thread_ts":thread,"status":status})),
        )?;
        Ok(())
    }
}
