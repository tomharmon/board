use super::*;
fn ts(s: &str) -> Result<()> {
    let (a, b) = s
        .split_once('.')
        .context("expected a Slack message timestamp")?;
    ensure!(
        !a.is_empty()
            && a.len() <= 16
            && !b.is_empty()
            && b.len() <= 9
            && a.bytes().chain(b.bytes()).all(|c| c.is_ascii_digit()),
        "invalid Slack timestamp"
    );
    Ok(())
}
pub(super) fn date(t: &str) -> Result<DateTime<Utc>> {
    ts(t)?;
    let (a, b) = t.split_once('.').unwrap();
    DateTime::from_timestamp(a.parse()?, format!("{b:0<9}").parse()?).context("invalid Slack time")
}
impl Connector {
    pub fn join_public_slack(&self, b: &Board, who: &str, name: &str) -> Result<()> {
        b.require_owner()?;
        let src = b.source(who, name)?;
        ensure!(
            self.provider == Provider::Slack && src.provider == Provider::Slack,
            "only Slack supports a public channel join"
        );
        let v = self.sl(
            Method::GET,
            "conversations.info",
            &[("channel", src.remote.clone())],
            None,
        )?;
        let c = &v["channel"];
        ensure!(
            c["id"] == src.remote
                && c["is_channel"] == true
                && c["is_private"] == false
                && c["is_shared"] == false
                && c["is_ext_shared"] != true
                && c["is_org_shared"] != true,
            "only the mapped public, non-shared Slack channel may be joined"
        );
        if c["is_member"] != true {
            self.sl(
                Method::POST,
                "conversations.join",
                &[],
                Some(&json!({"channel":src.remote})),
            )?;
        }
        Ok(())
    }
    fn sl(
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
        let mut out = Vec::new();
        for v in self.sl_pages(
            "conversations.replies",
            &[("channel", src.remote.clone()), ("ts", t.into())],
        )? {
            let id = s(&v, "ts")?;
            ts(id)?;
            let actor = v["user"]
                .as_str()
                .or(v["bot_id"].as_str())
                .unwrap_or("system")
                .to_owned();
            let text = v["text"]
                .as_str()
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
            let mut m = upsert(t, id.into(), actor, (id != t).then(|| t.into()), text);
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
        ensure!(
            out.iter().any(|e| e.import.external == t),
            "Slack thread root was not returned"
        );
        Ok(out)
    }
    pub(super) fn slack_send(
        &self,
        src: &Source,
        t: &str,
        d: &Delivery,
        copy: Option<&Copy>,
    ) -> Result<DeliveryResult> {
        if d.event == "thread.start" {
            let v=self.sl(Method::POST,"chat.postMessage",&[],Some(&json!({"channel":src.remote,"text":d.text,"unfurl_links":false,"unfurl_media":false})))?;
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
            let name = match emoji.as_str() {
                "thumbsup" => "+1",
                "check" => "white_check_mark",
                v => v,
            };
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
            let v=self.sl(Method::POST,"chat.postMessage",&[],Some(&json!({"channel":src.remote,"thread_ts":t,"text":d.text,"unfurl_links":false,"unfurl_media":false})))?;
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
                body["text"] = json!(d.text);
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
}
