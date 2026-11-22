use super::*;
use axum::extract::{Path, Query, RawQuery};
use base64::{Engine, engine::general_purpose::STANDARD};
use board_local::{Filter, PostOptions};
use serde::Deserialize;

type Data = ApiResult<Json<Value>>;

#[derive(Default, Deserialize, utoipa::IntoParams)]
#[serde(default, deny_unknown_fields)]
pub(super) struct ChannelQuery {
    after: Option<String>,
    scope: Option<String>,
    prefix: Option<String>,
    limit: Option<usize>,
}
#[utoipa::path(get, path = "/channels", params(ChannelQuery), tag = "channels")]
pub(super) async fn channels(State(a): State<App>, Query(q): Query<ChannelQuery>) -> Data {
    let (cursor, rows, next) = a
        .call(move |b, _| {
            b.channels_window(
                q.after.as_deref().unwrap_or(""),
                q.scope.as_deref(),
                q.prefix.as_deref(),
                q.limit.unwrap_or(100),
            )
        })
        .await?;
    Ok(Json(
        json!({"channels":rows,"cursor":cursor,"next_after":next}),
    ))
}
#[utoipa::path(get, path="/channels/{*name}", params(("name" = String, Path)), tag="channels")]
pub(super) async fn one_channel(State(a): State<App>, Path(name): Path<String>) -> Data {
    Ok(Json(json!(a.call(move |b, _| b.channel(&name)).await?)))
}
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub(super) struct ActorQuery {
    query: Option<String>,
    #[serde(default)]
    details: bool,
}
fn actor_query(raw: Option<String>) -> ApiResult<(ActorQuery, Vec<String>)> {
    let mut ids = Vec::new();
    let mut scalars = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in url::form_urlencoded::parse(raw.as_deref().unwrap_or("").as_bytes()) {
        if k == "id" {
            if ids.len() == 100 || board_local::validate_actor(&v).is_err() {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "at most 100 valid actor IDs",
                ));
            }
            ids.push(v.into_owned());
        } else {
            scalars.append_pair(&k, &v);
        }
    }
    let q = serde_urlencoded::from_str(&scalars.finish())
        .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid actors query"))?;
    Ok((q, ids))
}
#[utoipa::path(get, path="/actors", params(ActorQuery,("id"=Option<Vec<String>>, Query, description="Repeat for each actor, up to 100 IDs",style=Form,explode=true)), tag="actors")]
pub(super) async fn actors(State(a): State<App>, RawQuery(raw): RawQuery) -> Data {
    let (q, ids) = actor_query(raw)?;
    let (cursor, rows) = a
        .call(move |b, _| b.actor_directory_page(q.query.as_deref(), &ids))
        .await?;
    Ok(Json(if q.details {
        json!({"actors":rows,"cursor":cursor})
    } else {
        json!({"actors":rows.into_iter().map(|a|a.id).collect::<Vec<_>>(),"cursor":cursor})
    }))
}
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ActorProfile {
    handle: String,
    name: String,
    avatar: Option<String>,
}
#[utoipa::path(put, path = "/profile", tag = "profile")]
pub(super) async fn save_profile(
    State(a): State<App>,
    Json(p): Json<ActorProfile>,
) -> ApiResult<StatusCode> {
    a.call(move |b, who| {
        b.set_actor_profile_with_avatar(who, &p.handle, &p.name, p.avatar.as_deref())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct NewChannel {
    name: String,
    #[serde(default)]
    #[schema(default = false)]
    private: bool,
}
#[utoipa::path(post, path="/channels", responses((status=200, description="Public channel already exists"),(status=201, description="Channel created",headers(("Location"=String))),(status=403,description="Permission denied", body=Problem,content_type="application/problem+json"),(status=409,description="Private channel conflicts with an existing channel, subtree or grant",body=Problem,content_type="application/problem+json")), tag="channels")]
pub(super) async fn channel(
    State(a): State<App>,
    Json(body): Json<NewChannel>,
) -> ApiResult<Response> {
    let (name, new, grants) = a
        .call(move |b, actor| {
            if body.private {
                let grants = b.create_private_channel(&body.name, actor)?;
                Ok((body.name, true, Some(grants)))
            } else {
                let new = b.create_channel_as(&body.name, actor)?;
                Ok((body.name, new, None))
            }
        })
        .await?;
    let mut body = json!({"name":name,"created":new});
    if let Some(grants) = grants {
        body["grants"] = json!(grants);
    }
    Ok(if new {
        created(format!("/v1/channels/{name}"), body)
    } else {
        Json(body).into_response()
    })
}

#[derive(Default, Deserialize, utoipa::IntoParams)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Browse {
    channel: Option<String>,
    from: Option<String>,
    text: Option<String>,
    by: Option<String>,
    mentions: Option<String>,
    re: Option<i64>,
    after: Option<i64>,
    before: Option<i64>,
    #[param(minimum = 1, maximum = 500)]
    limit: Option<usize>,
}
impl Browse {
    fn parse(self) -> ApiResult<(Filter, usize)> {
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
                from: self.from,
                text: self.text,
                by: self.by,
                mentions: self.mentions,
                re: self.re,
                after: self.after,
                ..Default::default()
            },
            limit,
        ))
    }
}
fn resolve_me(f: &mut Filter, who: &str) {
    f.bind_actor(who);
}
#[utoipa::path(get, path="/threads", params(Browse), responses((status=200, description="Visible threads")), tag="threads")]
pub(super) async fn threads(State(a): State<App>, Query(q): Query<Browse>) -> Data {
    let before = q.before;
    let (mut f, n) = q.parse()?;
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
#[utoipa::path(get, path="/messages", params(Browse), responses((status=200, description="Visible messages")), tag="messages")]
pub(super) async fn messages(State(a): State<App>, Query(q): Query<Browse>) -> Data {
    let before = q.before;
    let (mut f, n) = q.parse()?;
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
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
pub(super) struct ThreadQuery {
    #[param(minimum = 1)]
    after: Option<i64>,
    #[param(minimum = 1, maximum = 500)]
    limit: Option<usize>,
}
#[utoipa::path(get, path="/threads/{id}", params(("id" = i64, Path), ThreadQuery), responses((status=200, description="Thread and readable replies")), tag="threads")]
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
#[utoipa::path(get, path="/posts/{key}", params(("key" = String, Path)), tag="posts")]
pub(super) async fn posted(State(a): State<App>, Path(key): Path<String>) -> Data {
    Ok(Json(json!(
        a.call(move |b, actor| b.posted(actor, &key)).await?
    )))
}
#[utoipa::path(get, path="/messages/{id}", params(("id" = i64, Path)), responses((status=200,description="Current message",body=board_local::Message)), tag="messages")]
pub(super) async fn message(State(a): State<App>, Path(id): Path<i64>) -> Data {
    Ok(Json(json!(a.call(move |b, _| b.get(id)).await?)))
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct NewMessage {
    #[serde(default)]
    no_sync: bool,
    #[serde(default)]
    #[schema(max_items = 32)]
    sync_targets: Vec<String>,
    channel: Option<String>,
    #[schema(minimum = 1)]
    reply_to: Option<i64>,
    #[serde(default)]
    #[schema(max_length = 10000)]
    text: String,
    after: Option<i64>,
    #[serde(default)]
    #[schema(max_items = 10)]
    attachments: Vec<FileInput>,
    #[serde(default)]
    #[schema(max_items = 10)]
    attachment_ids: Vec<super::files_api::FileRef>,
}
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct FileInput {
    name: String,
    #[schema(content_encoding = "base64")]
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
#[utoipa::path(post, path="/messages", params(("Idempotency-Key"=Option<String>,Header)), responses((status=200,description="Existing message",body=board_local::Message),(status=201,description="New message",body=board_local::Message,headers(("Location"=String)))), tag="messages")]
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
                no_sync: input.no_sync,
                key,
                after: input.after,
                files,
                attachment_ids: super::files_api::resolve(b, actor, &input.attachment_ids)?,
            };
            match (input.channel, input.reply_to) {
                (Some(c), None) => {
                    b.post_with_targets(&c, actor, &input.text, &opts, &input.sync_targets)
                }
                (None, Some(id)) => {
                    anyhow::ensure!(
                        input.sync_targets.is_empty(),
                        "sync_targets applies only to a new thread"
                    );
                    b.reply_with_outcome(id, actor, &input.text, &opts)
                }
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
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Edit {
    text: String,
    if_revision: Option<i64>,
}
#[utoipa::path(patch, path="/messages/{id}", params(("id" = i64, Path)), responses((status=200,description="Current message",body=board_local::Message)), tag="messages")]
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
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
pub(super) struct Revision {
    if_revision: Option<i64>,
}
#[utoipa::path(delete, path="/messages/{id}", params(("id" = i64, Path), Revision), tag="messages")]
pub(super) async fn remove(
    State(a): State<App>,
    Path(id): Path<i64>,
    Query(q): Query<Revision>,
) -> ApiResult<StatusCode> {
    a.call(move |b, actor| b.delete(id, actor, q.if_revision))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(put, path="/messages/{id}/reactions/{name}", params(("id" = i64, Path), ("name" = String, Path)), tag="messages")]
pub(super) async fn react(
    State(a): State<App>,
    Path((id, name)): Path<(i64, String)>,
) -> ApiResult<StatusCode> {
    a.call(move |b, actor| b.react(id, actor, &name, false))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(delete, path="/messages/{id}/reactions/{name}", params(("id" = i64, Path), ("name" = String, Path)), tag="messages")]
pub(super) async fn unreact(
    State(a): State<App>,
    Path((id, name)): Path<(i64, String)>,
) -> ApiResult<StatusCode> {
    a.call(move |b, actor| b.react(id, actor, &name, true))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(get, path="/attachments/{id}", params(("id" = String, Path)), tag="attachments")]
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
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
pub(super) struct EventQuery {
    #[serde(default)]
    all: bool,
    after: Option<i64>,
    from: Option<String>,
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
#[utoipa::path(get, path="/events", params(EventQuery,("Last-Event-ID"=Option<String>,Header,description="Resume after this delivered cursor")), tag="events")]
pub(super) async fn events(
    State(a): State<App>,
    headers: HeaderMap,
    Query(q): Query<EventQuery>,
) -> ApiResult<Response> {
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
    if q.after.is_some_and(|n| n < 0) || q.last.is_some_and(|n| n > 100) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "after must be nonnegative and last at most 100",
        ));
    }
    // A reconnect keeps its original URL. The delivered ID must not replay the initial seed.
    let pos = last_id.max(q.after);
    if q.last.is_some() && (q.after.is_some() || q.kind.is_some()) {
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
        from: q.from,
        re: q.re,
        by: q.by,
        text: q.text,
        mentions: q.mentions,
    }
    .parse()?;
    let (f, head) = a
        .call(move |b, who| {
            let mut f = f;
            resolve_me(&mut f, who);
            f.validate()?;
            Ok((f, b.cursor()?))
        })
        .await?;
    let mut after = pos.unwrap_or(head).min(head);
    let mut history = std::collections::VecDeque::new();
    let seed = q.last.filter(|_| last_id.is_none());
    if let Some(n) = seed {
        let filter = f.clone();
        let (h, rows) = a.call(move |b, _| b.tail_feed(n, &filter, q.all)).await?;
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
        return Ok(super::sse::stream(a, f, q.kind, q.all, after, history));
    }
    if seed.is_some() {
        return Ok(Json(json!({"cursor":after,"events":history})).into_response());
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(wait);
    loop {
        let filter = f.clone();
        let kind = q.kind.clone();
        let (pos, events) = a
            .call(move |b, _| b.event_feed(after, limit, &filter, kind.as_deref(), q.all))
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
#[utoipa::path(get, path = "/schemas", tag = "schemas")]
pub(super) async fn schemas(State(a): State<App>) -> Data {
    Ok(Json(json!({"schemas":a.call(|b,_|b.schemas()).await?})))
}
#[utoipa::path(get, path="/schemas/{name}/{version}", params(("name" = String, Path), ("version" = u32, Path)), tag="schemas")]
pub(super) async fn schema(State(a): State<App>, Path((name, v)): Path<(String, u32)>) -> Data {
    Ok(Json(json!(a.call(move |b, _| b.schema(&name, v)).await?)))
}
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct SchemaInput {
    #[serde(default)]
    description: String,
    schema: Value,
}
#[utoipa::path(put, path="/schemas/{name}/{version}", params(("name" = String, Path), ("version" = u32, Path)), tag="schemas")]
pub(super) async fn register_schema(
    State(a): State<App>,
    Path((name, v)): Path<(String, u32)>,
    Json(i): Json<SchemaInput>,
) -> ApiResult<Response> {
    let location = format!("/v1/schemas/{name}/{v}");
    let (new, schema) = a
        .call(move |b, actor| {
            Ok((
                b.register_schema_as(&name, v, &i.description, &i.schema, actor)?,
                b.schema(&name, v)?,
            ))
        })
        .await?;
    let reply = json!({"created":new,"schema":schema});
    Ok(if new {
        created(location, reply)
    } else {
        Json(reply).into_response()
    })
}
