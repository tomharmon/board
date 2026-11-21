use super::*;
use axum::{
    extract::{Path, Query},
    routing::post,
};
use board_local::sources::*;
use serde::Deserialize;

type Data = ApiResult<Json<Value>>;
pub(super) fn routes() -> Router<App> {
    Router::new()
        .route("/messages/{id}/sync", get(targets))
        .route("/links", get(locate))
        .route("/messages/{id}/deliveries", get(notices))
        .route("/sources", get(list).post(add))
        .route("/sources/{name}", get(one).patch(configure))
        .route(
            "/sources/{name}/renderer",
            get(renderer).put(set_renderer).delete(clear_renderer),
        )
        .route("/sources/{name}/imports", post(import))
        .route("/sources/{name}/links", put(link).get(find))
        .route(
            "/sources/{name}/thread-links",
            get(thread_link).put(join_thread),
        )
        .route("/sources/{name}/starts", post(start))
        .route("/sources/{name}/backfills", post(backfill))
        .route("/sources/{name}/files/{id}", get(files).post(share_file))
        .route("/sources/{name}/copies/{id}", get(copies))
        .route("/sources/{name}/inbox", get(inbox).post(push))
        .route("/sources/{name}/outbox", get(outbox))
        .route("/sources/{name}/deliveries", post(lease))
        .route("/sources/{name}/deliveries/{id}", get(item))
        .route("/sources/{name}/deliveries/{id}/receipt", put(ack))
        .route("/sources/{name}/deliveries/{id}/failure", post(fail))
        .route("/sources/{name}/deliveries/{id}/resolution", put(resolve))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Permalink {
    url: String,
}
async fn locate(State(a): State<App>, Query(q): Query<Permalink>) -> Data {
    Ok(Json(json!(
        a.call(move |b, _| b.locate_link(&q.url)).await?
    )))
}
async fn targets(State(a): State<App>, Path(id): Path<i64>) -> Data {
    Ok(Json(json!(a.call(move |b, _| b.sync_targets(id)).await?)))
}
async fn notices(State(a): State<App>, Path(id): Path<i64>) -> Data {
    Ok(Json(
        json!({"deliveries":a.call(move|b,u|b.message_deliveries(u,id)).await?}),
    ))
}
async fn list(State(a): State<App>) -> Data {
    Ok(Json(json!({"sources":a.call(|b,u|b.sources(u)).await?})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Renderer {
    script: String,
}
async fn renderer(State(a): State<App>, Path(n): Path<String>) -> Data {
    Ok(Json(
        json!({"script":a.call(move|b,u|b.source_renderer(u,&n)).await?}),
    ))
}
async fn set_renderer(
    State(a): State<App>,
    Path(n): Path<String>,
    Json(r): Json<Renderer>,
) -> ApiResult<Response> {
    let location = format!("/v1/sources/{n}/renderer");
    let new = a
        .call(move |b, u| b.set_source_renderer(u, &n, Some(&r.script)))
        .await?;
    Ok(if new {
        created(location, json!({"created":true}))
    } else {
        StatusCode::NO_CONTENT.into_response()
    })
}
async fn clear_renderer(State(a): State<App>, Path(n): Path<String>) -> ApiResult<StatusCode> {
    a.call(move |b, u| b.set_source_renderer(u, &n, None))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct New {
    name: String,
    provider: Provider,
    remote: String,
    channel: String,
}
async fn add(State(a): State<App>, Json(i): Json<New>) -> ApiResult<Response> {
    let (s, new) = a
        .call(move |b, u| b.create_source(u, &i.name, i.provider, &i.remote, &i.channel))
        .await?;
    Ok(if new {
        created(format!("/v1/sources/{}", s.name), json!(s))
    } else {
        Json(json!(s)).into_response()
    })
}
async fn one(State(a): State<App>, Path(n): Path<String>) -> Data {
    Ok(Json(json!(a.call(move |b, u| b.source(u, &n)).await?)))
}
async fn configure(
    State(a): State<App>,
    Path(n): Path<String>,
    Json(i): Json<SourcePatch>,
) -> Data {
    Ok(Json(json!(
        a.call(move |b, u| b.patch_source(u, &n, &i)).await?
    )))
}
async fn import(State(a): State<App>, Path(n): Path<String>, Json(e): Json<Envelope>) -> Data {
    Ok(Json(json!(
        a.call(move |b, u| b.import_source(u, &n, &e)).await?
    )))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Link {
    thread: String,
    external: String,
    message: i64,
    url: Option<String>,
}
async fn link(State(a): State<App>, Path(n): Path<String>, Json(i): Json<Link>) -> Data {
    Ok(Json(json!(
        a.call(move |b, u| b.attach_source(
            u,
            &n,
            &i.thread,
            &i.external,
            i.message,
            i.url.as_deref()
        ))
        .await?
    )))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lookup {
    thread: Option<String>,
    external: Option<String>,
    url: Option<String>,
}
async fn find(State(a): State<App>, Path(n): Path<String>, Query(q): Query<Lookup>) -> Data {
    let m = a
        .call(move |b, u| match (q.thread, q.url, q.external) {
            (Some(t), None, e) => b.find_source(u, &n, &t, e.as_deref()),
            (None, Some(url), None) => b.linked_source(u, &n, &url),
            _ => anyhow::bail!("specify thread and optional external, or url"),
        })
        .await?;
    Ok(Json(json!(m.ok_or_else(|| ApiError::new(
        StatusCode::NOT_FOUND,
        "no linked source message"
    ))?)))
}
async fn copies(State(a): State<App>, Path((n, id)): Path<(String, i64)>) -> Data {
    Ok(Json(
        json!({"copies":a.call(move|b,u|b.source_copies(u,&n,id)).await?}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    root: i64,
}
async fn thread_link(
    State(a): State<App>,
    Path(n): Path<String>,
    Query(q): Query<Backfill>,
) -> Data {
    Ok(Json(json!(
        a.call(move |b, u| b.source_thread_link(u, &n, &q.thread))
            .await?
    )))
}
async fn join_thread(
    State(a): State<App>,
    Path(n): Path<String>,
    Query(q): Query<Backfill>,
    Json(i): Json<Start>,
) -> ApiResult<Response> {
    let (link, fresh) = a
        .call(move |b, u| b.join_source_thread(u, &n, &q.thread, i.root))
        .await?;
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("thread", &link.thread)
        .finish();
    let loc = format!("/v1/sources/{}/thread-links?{query}", link.source);
    Ok((
        if fresh {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        [(header::LOCATION, loc)],
        Json(link),
    )
        .into_response())
}
async fn start(
    State(a): State<App>,
    Path(n): Path<String>,
    Json(i): Json<Start>,
) -> ApiResult<Response> {
    let j = a
        .call(move |b, u| b.start_source_thread(u, &n, i.root))
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        [(
            header::LOCATION,
            format!("/v1/sources/{}/deliveries/{}", j.source, j.id),
        )],
        Json(json!(j)),
    )
        .into_response())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Backfill {
    thread: String,
}
async fn backfill(State(a): State<App>, Path(n): Path<String>, Json(i): Json<Backfill>) -> Data {
    Ok(Json(
        json!({"queued":a.call(move|b,u|b.backfill_source(u,&n,&i.thread)).await?}),
    ))
}
async fn files(State(a): State<App>, Path((n, id)): Path<(String, i64)>) -> Data {
    Ok(Json(
        json!({"files":a.call(move|b,u|b.source_files(u,&n,id)).await?}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Share {
    thread: String,
    attachment: i64,
}
async fn share_file(
    State(a): State<App>,
    Path((n, id)): Path<(String, i64)>,
    Json(i): Json<Share>,
) -> ApiResult<Response> {
    let j = a
        .call(move |b, u| b.stage_source_file(u, &n, &i.thread, id, i.attachment))
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        [(
            header::LOCATION,
            format!("/v1/sources/{}/deliveries/{}", j.source, j.id),
        )],
        Json(json!(j)),
    )
        .into_response())
}
async fn push(
    State(a): State<App>,
    Path(n): Path<String>,
    Json(e): Json<Envelope>,
) -> ApiResult<Response> {
    let ids = a
        .call({
            let n = n.clone();
            move |b, u| b.enqueue_imports(u, &n, &[e], None)
        })
        .await?;
    let id = ids[0];
    Ok((
        StatusCode::ACCEPTED,
        [(header::LOCATION, format!("/v1/sources/{n}/deliveries/{id}"))],
        Json(json!({"id":id})),
    )
        .into_response())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    after: Option<i64>,
    limit: Option<usize>,
}
async fn queue(a: App, n: String, q: Page, d: Direction) -> Data {
    let pos = q.after.unwrap_or(0);
    let (cursor, rows) = a
        .call(move |b, u| b.inbox_page(u, &n, d, pos, q.limit.unwrap_or(50)))
        .await?;
    Ok(Json(json!({"cursor":cursor,"items":rows})))
}
async fn inbox(State(a): State<App>, Path(n): Path<String>, Query(q): Query<Page>) -> Data {
    queue(a, n, q, Direction::In).await
}
async fn outbox(State(a): State<App>, Path(n): Path<String>, Query(q): Query<Page>) -> Data {
    queue(a, n, q, Direction::Out).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Acquire {
    direction: Direction,
    lease_seconds: Option<i64>,
}
async fn lease(
    State(a): State<App>,
    Path(n): Path<String>,
    Json(i): Json<Acquire>,
) -> ApiResult<Response> {
    let got = a
        .call({
            let n = n.clone();
            move |b, u| b.take_source_delivery(u, &n, i.direction, i.lease_seconds.unwrap_or(60))
        })
        .await?;
    Ok(match got {
        Some(l) => created(format!("/v1/sources/{n}/deliveries/{}", l.job.id), json!(l)),
        None => StatusCode::NO_CONTENT.into_response(),
    })
}
async fn item(State(a): State<App>, Path((n, id)): Path<(String, i64)>) -> Data {
    Ok(Json(json!(
        a.call(move |b, u| b.queue_item(u, &n, id)).await?
    )))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ack {
    receipt: String,
    result: Option<DeliveryResult>,
}
async fn ack(State(a): State<App>, Path((n, id)): Path<(String, i64)>, Json(i): Json<Ack>) -> Data {
    Ok(Json(json!(
        a.call(move |b, u| b.ack_source(u, &n, id, &i.receipt, i.result.as_ref()))
            .await?
    )))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Failure {
    receipt: String,
    error: String,
    #[serde(default)]
    uncertain: bool,
    retry_after: Option<i64>,
}
async fn fail(
    State(a): State<App>,
    Path((n, id)): Path<(String, i64)>,
    Json(i): Json<Failure>,
) -> Data {
    Ok(Json(json!(
        a.call(move |b, u| b.fail_source(
            u,
            &n,
            id,
            &i.receipt,
            FailureReport {
                reason: &i.error,
                uncertain: i.uncertain,
                retry_after: i.retry_after
            }
        ))
        .await?
    )))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resolution {
    action: String,
    result: Option<DeliveryResult>,
}
async fn resolve(
    State(a): State<App>,
    Path((n, id)): Path<(String, i64)>,
    Json(i): Json<Resolution>,
) -> Data {
    Ok(Json(json!(
        a.call(move |b, u| b.resolve_source(u, &n, id, &i.action, i.result.as_ref()))
            .await?
    )))
}
