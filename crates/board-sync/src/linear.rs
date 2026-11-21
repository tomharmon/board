use super::*;

const ISSUE: &str = r#"query BoardIssue($id:String!, $after:String) {
  issue(id:$id) { id identifier title description url updatedAt priority priorityLabel project { name } assignee { name displayName } labels(first:250) { nodes { name } } creator { id name avatarUrl } team { id key } state { type name }
    comments(first:100, after:$after) { nodes { id body url updatedAt parent { id } user { id name avatarUrl } }
      pageInfo { hasNextPage endCursor } } }
}"#;
const ISSUES: &str = r#"query BoardIssues($filter:IssueFilter!, $after:String) {
  issues(filter:$filter, first:100, after:$after) { nodes { id } pageInfo { hasNextPage endCursor } }
}"#;
fn next(v: &Value, seen: &mut HashSet<String>) -> Result<Option<String>> {
    if !v["pageInfo"]["hasNextPage"]
        .as_bool()
        .context("invalid Linear pagination")?
    {
        return Ok(None);
    }
    let n = s(&v["pageInfo"], "endCursor")?;
    ensure!(
        !n.is_empty() && seen.insert(n.into()),
        "Linear returned a repeated or empty continuation"
    );
    Ok(Some(n.into()))
}
impl Connector {
    fn gql(&self, query: &str, vars: Value) -> Result<Value> {
        let v = self.req(
            Method::POST,
            "/graphql",
            &[],
            Some(&json!({"query":query,"variables":vars})),
        )?;
        ensure!(
            v["errors"].as_array().is_none_or(Vec::is_empty) && v["data"].is_object(),
            "Linear rejected the GraphQL operation"
        );
        Ok(v["data"].clone())
    }
    pub(super) fn linear_threads(&self, src: &Source, since: Option<&str>) -> Result<Vec<String>> {
        let mut filter = json!({"team":{"key":{"eq":src.remote}}});
        if let Some(at) = since {
            filter["updatedAt"] = json!({"gte":at});
        }
        let mut after = None;
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for _ in 0..1000 {
            let data = self.gql(ISSUES, json!({"filter":filter,"after":after}))?;
            let p = &data["issues"];
            for i in arr(p, "nodes")? {
                out.push(s(i, "id")?.into());
            }
            after = next(p, &mut seen)?;
            if after.is_none() {
                return Ok(out);
            }
        }
        bail!("Linear page ceiling reached; no checkpoint was advanced")
    }
    pub(super) fn linear_read(&self, src: &Source, thread: &str) -> Result<Vec<Envelope>> {
        let mut out = Vec::new();
        let mut after = None;
        let mut seen = HashSet::new();
        for _ in 0..1000 {
            let data = self.gql(ISSUE, json!({"id":thread,"after":after}))?;
            let i = &data["issue"];
            ensure!(
                i["team"]["key"] == src.remote,
                "Linear issue is outside configured team"
            );
            // UUID is canonical even when the command was given a human-readable issue identifier.
            let t = s(i, "id")?;
            let issue = s(i, "identifier")?;
            if out.is_empty() {
                let mut root = upsert(
                    t,
                    "issue".into(),
                    i["creator"]["id"].as_str().unwrap_or("system").into(),
                    None,
                    content(&format!("{issue}: {}", s(i, "title")?), &i["description"]),
                );
                root.url = Some(s(i, "url")?.into());
                root.updated_at = stamp(i, "updatedAt")?;
                labels(&mut root, &[("issue", issue)]);
                linear_properties(&mut root, i);
                let mut raw = i.clone();
                raw.as_object_mut().unwrap().remove("comments");
                out.push(envelope(root, raw));
            }
            let comments = &i["comments"];
            for c in arr(comments, "nodes")? {
                let mut m = upsert(
                    t,
                    format!("comment:{}", s(c, "id")?),
                    c["user"]["id"].as_str().unwrap_or("system").into(),
                    Some(
                        c["parent"]["id"]
                            .as_str()
                            .map(|p| format!("comment:{p}"))
                            .unwrap_or_else(|| "issue".into()),
                    ),
                    s(c, "body")?.to_owned(),
                );
                m.url = c["url"].as_str().map(str::to_owned);
                m.updated_at = stamp(c, "updatedAt")?;
                out.push(envelope(m, c.clone()));
            }
            after = next(comments, &mut seen)?;
            if after.is_none() {
                return Ok(out);
            }
        }
        bail!("Linear page ceiling reached; no checkpoint was advanced")
    }
    pub(super) fn linear_send(
        &self,
        src: &Source,
        t: &str,
        d: &Delivery,
        copy: Option<&Copy>,
        parent: Option<&Copy>,
    ) -> Result<DeliveryResult> {
        if let Some(emoji) = &d.emoji {
            let c = copy.context("no Linear copy to react to")?;
            let comment = c
                .external
                .strip_prefix("comment:")
                .context("expected a Linear comment")?;
            let data=self.gql("mutation BoardReact($input:ReactionCreateInput!){reactionCreate(input:$input){success}}",json!({"input":{"commentId":comment,"emoji":emoji}}))?;
            ensure!(
                data["reactionCreate"]["success"] == true,
                "Linear did not confirm the reaction"
            );
            return Ok(DeliveryResult {
                thread: None,
                external: c.external.clone(),
                url: c.url.clone(),
            });
        }
        if d.event == "thread.start" {
            let data = self.gql(
                "query BoardTeam($key:String!){teams(filter:{key:{eq:$key}},first:2){nodes{id}}}",
                json!({"key":src.remote}),
            )?;
            let teams = arr(&data["teams"], "nodes")?;
            ensure!(
                teams.len() == 1,
                "could not identify exactly one Linear team"
            );
            let mut lines = d.text.lines();
            let title = lines
                .next()
                .filter(|x| !x.is_empty())
                .unwrap_or("Board thread");
            let created=self.gql("mutation BoardStart($input:IssueCreateInput!){issueCreate(input:$input){success issue{id url}}}",json!({"input":{"teamId":s(&teams[0],"id")?,"title":title,"description":lines.collect::<Vec<_>>().join("\n")}}))?;
            ensure!(
                created["issueCreate"]["success"] == true,
                "Linear did not accept the issue"
            );
            let i = &created["issueCreate"]["issue"];
            return Ok(DeliveryResult {
                thread: Some(s(i, "id")?.into()),
                external: "issue".into(),
                url: i["url"].as_str().map(str::to_owned),
            });
        }
        if d.event == "message.posted" {
            let mut input = json!({"issueId":t,"body":d.text});
            if let Some(p) = parent.and_then(|x| x.external.strip_prefix("comment:")) {
                input["parentId"] = json!(p);
            }
            let v=self.gql("mutation BoardReply($input:CommentCreateInput!){commentCreate(input:$input){success comment{id url}}}",json!({"input":input}))?;
            let created = &v["commentCreate"];
            ensure!(
                created["success"] == true,
                "Linear did not accept the comment"
            );
            let c = &created["comment"];
            return Ok(DeliveryResult {
                thread: None,
                external: format!("comment:{}", s(c, "id")?),
                url: c["url"].as_str().map(str::to_owned),
            });
        }
        let c = copy.context("no Linear copy to change")?;
        ensure!(!c.origin, "only a connector's own messages may be changed");
        if c.external == "issue" {
            let (title, body) = super::root_text(d);
            let v=self.gql("mutation BoardUpdateIssue($id:String!,$input:IssueUpdateInput!){issueUpdate(id:$id,input:$input){success}}",json!({"id":t,"input":{"title":title,"description":body}}))?;
            ensure!(
                v["issueUpdate"]["success"] == true,
                "Linear did not accept the issue update"
            );
            return Ok(DeliveryResult {
                thread: None,
                external: c.external.clone(),
                url: c.url.clone(),
            });
        }
        let id = c
            .external
            .strip_prefix("comment:")
            .context("expected a Linear comment")?;
        let success = if d.event == "message.deleted" {
            self.gql(
                "mutation BoardRemove($id:String!){commentDelete(id:$id){success}}",
                json!({"id":id}),
            )?["commentDelete"]["success"]
                .clone()
        } else {
            self.gql("mutation BoardEdit($id:String!,$input:CommentUpdateInput!){commentUpdate(id:$id,input:$input){success}}",json!({"id":id,"input":{"body":d.text}}))?["commentUpdate"]["success"].clone()
        };
        ensure!(success == true, "Linear did not accept the change");
        Ok(DeliveryResult {
            thread: None,
            external: c.external.clone(),
            url: c.url.clone(),
        })
    }
}
pub(super) fn linear_properties(m: &mut Import, i: &Value) {
    if let Some(state) = i["state"]["type"].as_str().or(i["state"]["name"].as_str()) {
        labels(m, &[("state", &state.to_lowercase())]);
    }
    if let Some(p) = i["priorityLabel"].as_str() {
        labels(m, &[("priority", &p.to_lowercase())]);
    }
    if let Some(p) = i["project"]["name"].as_str() {
        labels(m, &[("project", p)]);
    }
    if let Some(a) = i["assignee"]["displayName"]
        .as_str()
        .or(i["assignee"]["name"].as_str())
    {
        property(m, "assignee", a);
    }
    if let Some(list) = i["labels"]["nodes"].as_array().or(i["labels"].as_array()) {
        for t in list {
            if let Some(k) = t["name"].as_str() {
                property(m, "tag", k);
            }
        }
    }
}
