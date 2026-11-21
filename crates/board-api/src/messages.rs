use super::*;
use axum::extract::{Path, Query, RawQuery};
use base64::{Engine, engine::general_purpose::STANDARD};
use board_local::{Filter, PostOptions};
use serde::Deserialize;
use std::collections::BTreeMap;

type Labels = BTreeMap<String, String>;
type Data = ApiResult<Json<Value>>;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct ChannelQuery {
    after: Option<String>,
    scope: Option<String>,
    limit: Option<usize>,
}
pub(super) async fn channels(State(a): State<App>, Query(q): Query<ChannelQuery>) -> Data {
    let (cursor, rows, next) = a
        .call(move |b, _| {
            b.channel_page(
                q.after.as_deref().unwrap_or(""),
                q.scope.as_deref(),
                q.limit.unwrap_or(50),
            )
        })
        .await?;
    Ok(Json(
        json!({"channels":rows,"cursor":cursor,"next_after":next}),
    ))
}
pub(super) async fn one_channel(State(a): State<App>, Path(name): Path<String>) -> Data {
    Ok(Json(json!(a.call(move |b, _| b.channel(&name)).await?)))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ActorQuery {
    query: Option<String>,
    id: Option<String>,
    #[serde(default)]
    details: bool,
}
pub(super) async fn actors(State(a): State<App>, Query(q): Query<ActorQuery>) -> Data {
    let rows = a
        .call(move |b, _| {
            let mut rows = b.actor_directory(q.query.as_deref())?;
            if let Some(id) = q.id {
                rows.retain(|a| a.id == id);
            }
            Ok(rows)
        })
        .await?;
    Ok(Json(if q.details {
        json!({"actors":rows})
    } else {
        json!({"actors":rows.into_iter().map(|a|a.id).collect::<Vec<_>>()})
    }))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ActorProfile {
    handle: String,
    name: String,
}
pub(super) async fn save_profile(
    State(a): State<App>,
    Json(p): Json<ActorProfile>,
) -> ApiResult<StatusCode> {
    a.call(move |b, who| b.set_actor_profile(who, &p.handle, &p.name))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NewChannel {
    name: String,
}
pub(super) async fn channel(
    State(a): State<App>,
    Json(body): Json<NewChannel>,
) -> ApiResult<Response> {
    let (name, new) = a
        .call(move |b, actor| {
            let new = b.create_channel_as(&body.name, actor)?;
            Ok((body.name, new))
        })
        .await?;
    let body = json!({"name":name,"created":new});
    Ok(if new {
        created(format!("/v1/channels/{name}"), body)
    } else {
        Json(body).into_response()
    })
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Browse {
    channel: Option<String>,
    text: Option<String>,
    by: Option<String>,
    mentions: Option<String>,
    re: Option<i64>,
    after: Option<i64>,
    before: Option<i64>,
    limit: Option<usize>,
}
impl Browse {
    fn parse(self, terms: &[String]) -> ApiResult<(Filter, usize)> {
        let limit = self.limit.unwrap_or(50);
        if !(1..=500).contains(&limit) || self.after.is_some_and(|n| n < 0) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "limit must be 1..500 and after nonnegative",
            ));
        }
        Ok((
            Filter {
                channel: self.channel,
                text: self.text,
                by: self.by,
                mentions: self.mentions,
                re: self.re,
                after: self.after,
                labels: Default::default(),
                label_terms: board_local::LabelTerm::parse(terms)?,
            },
            limit,
        ))
    }
}
fn query_terms<T: serde::de::DeserializeOwned>(raw: Option<String>) -> ApiResult<(T, Vec<String>)> {
    let mut labels = Vec::new();
    let mut scalars = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in url::form_urlencoded::parse(raw.as_deref().unwrap_or("").as_bytes()) {
        if k == "label" {
            labels.push(v.into_owned());
        } else {
            scalars.append_pair(&k, &v);
        }
    }
    let q = serde_urlencoded::from_str(&scalars.finish()).map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid or unknown query parameter",
        )
    })?;
    Ok((q, labels))
}
fn resolve_me(f: &mut Filter, who: &str) {
    if f.by.as_deref() == Some("me") {
        f.by = Some(who.into());
    }
    if matches!(f.mentions.as_deref(), Some("" | "me")) {
        f.mentions = Some(who.into());
    }
}
pub(super) async fn threads(State(a): State<App>, RawQuery(raw): RawQuery) -> Data {
    let (q, terms) = query_terms::<Browse>(raw)?;
    let before = q.before;
    let (mut f, n) = q.parse(&terms)?;
    let p = a
        .call(move |b, who| {
            resolve_me(&mut f, who);
            b.threads_window(&f, before, n)
        })
        .await?;
    Ok(Json(
        json!({"cursor":p.cursor,"snapshot":p.snapshot,"older":p.older,"threads":p.rows}),
    ))
}
pub(super) async fn messages(State(a): State<App>, RawQuery(raw): RawQuery) -> Data {
    let (q, terms) = query_terms::<Browse>(raw)?;
    let before = q.before;
    let (mut f, n) = q.parse(&terms)?;
    let p = a
        .call(move |b, who| {
            resolve_me(&mut f, who);
            b.messages_window(&f, before, n)
        })
        .await?;
    Ok(Json(
        json!({"cursor":p.cursor,"snapshot":p.snapshot,"older":p.older,"messages":p.rows}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ThreadQuery {
    after: Option<i64>,
    limit: Option<usize>,
}
pub(super) async fn thread(
    State(a): State<App>,
    Path(id): Path<i64>,
    Query(q): Query<ThreadQuery>,
) -> Data {
    Ok(Json(json!(
        a.call(move |b, _| b.conversation_window(
            id,
            &Filter {
                after: q.after,
                ..Filter::default()
            },
            q.limit
        ))
        .await?
    )))
}
pub(super) async fn posted(State(a): State<App>, Path(key): Path<String>) -> Data {
    Ok(Json(json!(
        a.call(move |b, actor| b.posted(actor, &key)).await?
    )))
}
pub(super) async fn message(State(a): State<App>, Path(id): Path<i64>) -> Data {
    Ok(Json(json!(a.call(move |b, _| b.get(id)).await?)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NewMessage {
    channel: Option<String>,
    reply_to: Option<i64>,
    #[serde(default)]
    text: String,
    after: Option<i64>,
    #[serde(default)]
    labels: Labels,
    #[serde(default)]
    attachments: Vec<FileInput>,
    #[serde(default)]
    attachment_ids: Vec<super::files_api::FileRef>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileInput {
    name: String,
    data_base64: String,
}
pub(super) fn decode_files(attachments: Vec<FileInput>) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
    anyhow::ensure!(attachments.len() <= 10, "at most 10 attachments");
    attachments
        .into_iter()
        .map(|file| {
            anyhow::ensure!(
                file.data_base64.len() <= 5_592_408,
                "attachment exceeds 4 MiB"
            );
            let bytes = STANDARD
                .decode(file.data_base64)
                .map_err(|_| anyhow::anyhow!("invalid attachment base64"))?;
            Ok((file.name, bytes))
        })
        .collect()
}
pub(super) async fn post_message(
    State(a): State<App>,
    headers: HeaderMap,
    Json(input): Json<NewMessage>,
) -> ApiResult<Response> {
    let key = idempotency(&headers)?;
    let (m, new) = a
        .call(move |b, actor| {
            let files = decode_files(input.attachments)?;
            let opts = PostOptions {
                key,
                after: input.after,
                labels: input.labels,
                files,
                attachment_ids: super::files_api::resolve(b, actor, &input.attachment_ids)?,
            };
            match (input.channel, input.reply_to) {
                (Some(c), None) => b.post_with_outcome(&c, actor, &input.text, &opts),
                (None, Some(id)) => b.reply_with_outcome(id, actor, &input.text, &opts),
                _ => anyhow::bail!("provide exactly one of channel or reply_to"),
            }
        })
        .await?;
    Ok(if new {
        created(format!("/v1/messages/{}", m.id), json!(m))
    } else {
        Json(json!(m)).into_response()
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Edit {
    text: String,
    if_revision: Option<i64>,
}
pub(super) async fn edit(
    State(a): State<App>,
    Path(id): Path<i64>,
    Json(body): Json<Edit>,
) -> Data {
    Ok(Json(json!(
        a.call(move |b, actor| b.edit(id, actor, &body.text, body.if_revision))
            .await?
    )))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Revision {
    if_revision: Option<i64>,
}
pub(super) async fn remove(
    State(a): State<App>,
    Path(id): Path<i64>,
    Query(q): Query<Revision>,
) -> ApiResult<StatusCode> {
    a.call(move |b, actor| b.delete(id, actor, q.if_revision))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub(super) struct LabelPatch {
    set: Labels,
    unset: Vec<String>,
    conditions: BTreeMap<String, Option<String>>,
}
pub(super) async fn labels(
    State(a): State<App>,
    Path(id): Path<i64>,
    Json(p): Json<LabelPatch>,
) -> Data {
    Ok(Json(json!(
        a.call(move |b, actor| b.labels(id, actor, &p.set, &p.unset, &p.conditions))
            .await?
    )))
}
pub(super) async fn react(
    State(a): State<App>,
    Path((id, name)): Path<(i64, String)>,
) -> ApiResult<StatusCode> {
    a.call(move |b, actor| b.react(id, actor, &name, false))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
pub(super) async fn unreact(
    State(a): State<App>,
    Path((id, name)): Path<(i64, String)>,
) -> ApiResult<StatusCode> {
    a.call(move |b, actor| b.react(id, actor, &name, true))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
pub(super) async fn attachment(
    State(a): State<App>,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let (file, bytes) = a
        .call(move |b, who| b.download_as(who, b.file_id(who, &id)?))
        .await?;
    Ok((
        [
            (header::CONTENT_TYPE, file.content_type.as_str()),
            (header::CONTENT_DISPOSITION, "attachment"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (
                header::CONTENT_SECURITY_POLICY,
                "sandbox; default-src 'none'",
            ),
        ],
        bytes,
    )
        .into_response())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EventQuery {
    after: Option<i64>,
    limit: Option<usize>,
    channel: Option<String>,
    re: Option<i64>,
    by: Option<String>,
    text: Option<String>,
    mentions: Option<String>,
    kind: Option<String>,
    wait_ms: Option<u64>,
    last: Option<usize>,
}
pub(super) async fn events(
    State(a): State<App>,
    headers: HeaderMap,
    RawQuery(raw): RawQuery,
) -> ApiResult<Response> {
    let (q, terms) = query_terms::<EventQuery>(raw)?;
    let last_id = headers
        .get("last-event-id")
        .map(|h| {
            h.to_str()
                .ok()
                .and_then(|s| s.parse::<i64>().ok())
                .filter(|x| *x >= 0)
                .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "invalid Last-Event-ID"))
        })
        .transpose()?;
    let pos = q.after.or(last_id);
    if q.last.is_some() && (pos.is_some() || q.kind.is_some()) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "last cannot be combined with a cursor or kind",
        ));
    }
    let wait = q.wait_ms.unwrap_or(0);
    if wait > 30_000 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "wait_ms must be at most 30000",
        ));
    }
    let (f, limit) = Browse {
        after: None,
        before: None,
        limit: q.limit,
        channel: q.channel,
        re: q.re,
        by: q.by,
        text: q.text,
        mentions: q.mentions,
    }
    .parse(&terms)?;
    let (f, head) = a
        .call(move |b, who| {
            let mut f = f;
            resolve_me(&mut f, who);
            Ok((f, b.cursor()?))
        })
        .await?;
    if pos.is_some_and(|n| n < 0) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "after must be nonnegative",
        ));
    }
    let mut after = pos.unwrap_or(head).min(head);
    let mut history = std::collections::VecDeque::new();
    if let Some(n) = q.last {
        let filter = f.clone();
        let (h, rows) = a.call(move |b, _| b.event_tail(n, &filter)).await?;
        after = h;
        history = rows.into();
    }
    if headers
        .get(header::ACCEPT)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|s| {
            s.split(',').any(|p| {
                p.split(';')
                    .next()
                    .is_some_and(|v| v.trim() == "text/event-stream")
            })
        })
    {
        return Ok(super::sse::stream(a, f, q.kind, after, history));
    }
    if q.last.is_some() {
        return Ok(Json(json!({"cursor":after,"events":history})).into_response());
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(wait);
    loop {
        let filter = f.clone();
        let kind = q.kind.clone();
        let (pos, events) = a
            .call(move |b, _| b.event_page(after, limit, &filter, kind.as_deref()))
            .await?;
        after = pos;
        if !events.is_empty() || tokio::time::Instant::now() >= deadline {
            return Ok(Json(json!({"cursor":pos,"events":events})).into_response());
        }
        tokio::time::sleep_until(
            deadline.min(tokio::time::Instant::now() + std::time::Duration::from_millis(100)),
        )
        .await;
    }
}
pub(super) async fn schemas(State(a): State<App>) -> Data {
    Ok(Json(json!({"schemas":a.call(|b,_|b.schemas()).await?})))
}
pub(super) async fn schema(State(a): State<App>, Path((name, v)): Path<(String, u32)>) -> Data {
    let schemas = a.call(|b, _| b.schemas()).await?;
    Ok(Json(json!(
        schemas
            .into_iter()
            .find(|s| s.name == name && s.version == v)
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "schema not found"))?
    )))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SchemaInput {
    #[serde(default)]
    description: String,
    schema: Value,
}
pub(super) async fn register_schema(
    State(a): State<App>,
    Path((name, v)): Path<(String, u32)>,
    Json(i): Json<SchemaInput>,
) -> ApiResult<Response> {
    let location = format!("/v1/schemas/{name}/{v}");
    let new = a
        .call(move |b, actor| b.register_schema_as(&name, v, &i.description, &i.schema, actor))
        .await?;
    Ok(if new {
        created(location, json!({"created":true}))
    } else {
        StatusCode::NO_CONTENT.into_response()
    })
}
