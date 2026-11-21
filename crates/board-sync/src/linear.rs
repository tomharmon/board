use super::*;

const ISSUE: &str = r#"query BoardIssue($id:String!, $after:String,$first:Int!) {
  issue(id:$id) { id identifier title description url updatedAt priority priorityLabel project { name } assignee { name displayName } labels(first:250) { nodes { name } } creator { id name avatarUrl } team { id key } state { type name }
    comments(first:$first, after:$after) { nodes { id body url updatedAt parent { id } user { id name avatarUrl } }
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
    /// Recheck the issue on every send: a durable copy may predate a team move.
    pub(super) fn linear_destination(
        &self,
        src: &Source,
        thread: &str,
        comment: Option<&str>,
    ) -> Result<String> {
        let data = if let Some(c) = comment {
            self.gql("query BoardCommentDestination($id:String!,$comment:String!){issue(id:$id){id identifier team{key}} comment(id:$comment){id issue{id}}}",json!({"id":thread,"comment":c}))?
        } else {
            self.gql(
                "query BoardIssueDestination($id:String!){issue(id:$id){id identifier team{key}}}",
                json!({"id":thread}),
            )?
        };
        let issue = &data["issue"];
        ensure!(
            issue["team"]["key"] == src.remote,
            "Linear issue is outside the configured team or inaccessible"
        );
        let id = s(issue, "id")?;
        ensure!(
            id == thread
                || issue["identifier"]
                    .as_str()
                    .is_some_and(|alias| alias.eq_ignore_ascii_case(thread)),
            "Linear returned a different issue"
        );
        if let Some(c) = comment {
            ensure!(
                data["comment"]["id"] == c && data["comment"]["issue"]["id"] == id,
                "Linear comment is outside the verified issue or inaccessible"
            );
        }
        Ok(id.to_owned())
    }
    pub(super) fn linear_preview(
        &self,
        src: &Source,
        target: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Value> {
        let mut filter = json!({"team":{"key":{"eq":src.remote}}});
        let mut issue = target
            .filter(|t| !t.eq_ignore_ascii_case(&src.remote))
            .map(str::to_owned);
        if let Some(raw) = target.filter(|t| t.contains("://")) {
            let u = provider_url(raw, "linear.app")?;
            let path: Vec<_> = u.path_segments().unwrap().collect();
            let mut pairs = path.windows(2);
            let pair = pairs
                .find(|p| matches!(p[0], "issue" | "project"))
                .context("expected a Linear issue or project link")?;
            if pair[0] == "issue" {
                issue = Some(pair[1].to_uppercase());
            } else {
                let slug = pair[1].rsplit('-').next().unwrap_or("");
                ensure!(
                    (8..=64).contains(&slug.len()) && slug.bytes().all(|b| b.is_ascii_hexdigit()),
                    "invalid Linear project identifier"
                );
                let p=self.gql("query BoardProject($slug:String!){projects(filter:{slugId:{eq:$slug}},first:1){nodes{id name}}}",json!({"slug":slug}))?;
                let project = arr(&p["projects"], "nodes")?
                    .first()
                    .context("Linear project was not found")?;
                filter["project"] = json!({"id":{"eq":s(project,"id")?}});
                issue = None;
            }
        }
        if let Some(id) = issue {
            ensure!(
                id.len() <= 128
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
                "invalid Linear issue identifier"
            );
            let mut page = Vec::new();
            let mut seen = HashSet::new();
            let next = self.linear_issue_page(src, &id, cursor, limit, &mut seen, &mut page)?;
            let records = page.into_iter().map(|e| e.import).collect::<Vec<_>>();
            return Ok(json!({"thread":id,"records":records,"next":next}));
        }
        let data=self.gql("query BoardPreview($filter:IssueFilter!,$after:String,$limit:Int!){issues(filter:$filter,first:$limit,after:$after){nodes{id identifier title description state{name} url creator{name}} pageInfo{hasNextPage endCursor}}}",json!({"filter":filter,"after":cursor,"limit":limit}))?;
        let p = &data["issues"];
        let ids = arr(p, "nodes")?
            .iter()
            .map(|v| id(v, "id"))
            .collect::<Result<Vec<_>>>()?;
        let records=arr(p,"nodes")?.iter().map(|v|json!({"thread":v["id"],"identifier":v["identifier"],"title":v["title"],"text":v["description"].as_str().unwrap_or("").chars().take(1500).collect::<String>(),"url":v["url"],"state":v["state"]["name"],"author":v["creator"]["name"]})).collect::<Vec<_>>();
        Ok(json!({"threads":ids,"records":records,"next":next(p,&mut HashSet::new())?}))
    }
    pub(super) fn gql(&self, query: &str, vars: Value) -> Result<Value> {
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
            after =
                self.linear_issue_page(src, thread, after.as_deref(), 100, &mut seen, &mut out)?;
            if after.is_none() {
                return Ok(out);
            }
        }
        bail!("Linear page ceiling reached; no checkpoint was advanced")
    }
    fn linear_issue_page(
        &self,
        src: &Source,
        thread: &str,
        cursor: Option<&str>,
        limit: usize,
        seen: &mut HashSet<String>,
        out: &mut Vec<Envelope>,
    ) -> Result<Option<String>> {
        let data = self.gql(ISSUE, json!({"id":thread,"after":cursor,"first":limit}))?;
        let i = &data["issue"];
        ensure!(
            i["team"]["key"] == src.remote,
            "Linear issue is outside configured team"
        );
        // UUID is canonical even when the command was given a human-readable issue identifier.
        let t = s(i, "id")?;
        let issue = s(i, "identifier")?;
        if cursor.is_none() {
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
        next(comments, seen)
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
            if d.event == "message.deleted" {
                return Ok(DeliveryResult {
                    thread: None,
                    external: c.external.clone(),
                    url: c.url.clone(),
                });
            }
            let v=self.gql("mutation BoardUpdateIssue($id:String!,$input:IssueUpdateInput!){issueUpdate(id:$id,input:$input){success}}",json!({"id":t,"input":{"description":d.text}}))?;
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
    let mut owns = Vec::new();
    if let Some(p) = i["priorityLabel"]
        .as_str()
        .filter(|v| *v != "No priority" && i["priority"] != 0)
    {
        let slug = property_slug(p);
        if !slug.is_empty() {
            labels(m, &[("priority", &slug)]);
        }
    }
    if i["priority"] == 0 || i["priorityLabel"] == "No priority" {
        owns.push("priority".into());
    }
    if let Some(p) = i["project"]["name"].as_str() {
        let slug = property_slug(p);
        if !slug.is_empty() {
            labels(m, &[("project", &slug)]);
        }
    } else if i["project"].is_null() && i["projectId"].is_null() {
        owns.push("project".into());
    }
    if let Some(a) = i["assignee"]["displayName"]
        .as_str()
        .or(i["assignee"]["name"].as_str())
    {
        property(m, "assignee", a);
    }
    if !i["assignee"].is_null() || i["assigneeId"].is_null() {
        owns.push("assignee.".into());
    }
    if let Some(list) = i["labels"]["nodes"].as_array().or(i["labels"].as_array()) {
        owns.push("tag.".into());
        for t in list {
            if let Some(k) = t["name"].as_str() {
                property(m, "tag", k);
            }
        }
    }
    if let Change::Upsert { text, labels } = &m.change {
        m.change = Change::Reconcile {
            text: text.clone(),
            labels: labels.clone(),
            owns,
            reset: BTreeMap::new(),
            existing_only: false,
        };
    }
}
