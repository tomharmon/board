//! Local work resources. Database mutations and their preconditions live in board-local.
use super::messages::{FileInput, decode_files};
use super::{ApiError, ApiResult, App, created, idempotency};
use axum::{
    Json,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use board_local::{
    PostOptions,
    automation::{self as work, Task, TaskAction, TaskTarget},
};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Deserialize;
use serde_json::{Value, json};
use utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouterExt};
type Data = ApiResult<Json<Value>>;

pub(super) fn routes() -> OpenApiRouter<App> {
    OpenApiRouter::new()
        .routes(
            utoipa_axum::routes!(tasks, create_task).layer(DefaultBodyLimit::max(56 * 1024 * 1024)),
        )
        .routes(utoipa_axum::routes!(task, update_task))
        .routes(utoipa_axum::routes!(assignment))
        .routes(utoipa_axum::routes!(dependencies))
        .routes(utoipa_axum::routes!(get_claim, claim, renew, release))
        .routes(utoipa_axum::routes!(recover))
        .routes(utoipa_axum::routes!(create_batch, batch_query))
        .routes(utoipa_axum::routes!(batch))
        .routes(utoipa_axum::routes!(watchers, watcher, replace_watchers))
        .routes(utoipa_axum::routes!(matches))
        .routes(utoipa_axum::routes!(get_watcher, remove_watcher))
        .routes(utoipa_axum::routes!(peek))
        .routes(utoipa_axum::routes!(deliver))
        .routes(utoipa_axum::routes!(renew_delivery))
        .routes(utoipa_axum::routes!(failures))
        .routes(utoipa_axum::routes!(ack))
}

fn invalid(msg: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, msg)
}
fn limit(n: Option<usize>) -> ApiResult<usize> {
    let n = n.unwrap_or(50);
    if (1..=500).contains(&n) {
        Ok(n)
    } else {
        Err(invalid("limit must be 1..500"))
    }
}
fn after(n: Option<i64>) -> ApiResult<i64> {
    let n = n.unwrap_or_default();
    if n >= 0 {
        Ok(n)
    } else {
        Err(invalid("after must be nonnegative"))
    }
}

// Fetching one extra row in the same snapshot avoids a spurious empty last page.
fn continuation<T, K>(rows: &mut Vec<T>, n: usize, key: impl FnOnce(&T) -> K) -> Option<K> {
    if rows.len() <= n {
        return None;
    }
    rows.truncate(n);
    rows.last().map(key)
}

#[derive(Default, Deserialize, utoipa::IntoParams)]
#[serde(default, deny_unknown_fields)]
struct TaskPage {
    latest: bool,
    channel: Option<String>,
    state: Option<String>,
    needs: Option<i64>,
    by: Option<String>,
    owner: Option<String>,
    #[serde(rename = "for")]
    assignee: Option<String>,
    after: Option<i64>,
    limit: Option<usize>,
}
#[utoipa::path(get, path = "/tasks", params(TaskPage), tag = "tasks")]
async fn tasks(State(app): State<App>, Query(q): Query<TaskPage>) -> Data {
    if q.latest && q.after.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "latest and after cannot be combined",
        ));
    }
    let (after, limit) = (after(q.after)?, limit(q.limit)?);
    let (cursor, mut rows) = app
        .call(move |b, actor| {
            work::filtered_tasks_page_as(
                b,
                Some(actor),
                &work::TaskFilter {
                    latest: q.latest,
                    channel: q.channel,
                    state: q.state,
                    needs: q.needs,
                    by: q.by,
                    owner: q.owner,
                    assignee: q.assignee,
                },
                after,
                limit + usize::from(!q.latest),
            )
        })
        .await?;
    let next = continuation(&mut rows, limit, |t| t.id);
    Ok(Json(
        json!({"cursor":cursor,"next_after":next,"tasks":rows}),
    ))
}
#[utoipa::path(get, path="/tasks/{id}", params(("id" = i64, Path)), tag="tasks")]
async fn task(State(app): State<App>, Path(id): Path<i64>) -> Data {
    Ok(Json(json!(
        app.call(move |b, _| work::get_task(b, id)).await?
    )))
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct BatchInput {
    channel: String,
    run: Option<String>,
    #[schema(min_items = 1, max_items = 100)]
    tasks: Vec<work::PlanItem>,
}
#[utoipa::path(post, path = "/task-batches", tag = "task-batches")]
async fn create_batch(
    State(app): State<App>,
    headers: HeaderMap,
    Json(b): Json<BatchInput>,
) -> ApiResult<Response> {
    let key = idempotency(&headers)?.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let encoded = utf8_percent_encode(&key, NON_ALPHANUMERIC);
    // HTTP clients normalize a standalone dot segment even when it is percent-encoded.
    let location = if matches!(key.as_str(), "." | "..") {
        format!("/v1/task-batches?key={encoded}")
    } else {
        format!("/v1/task-batches/{encoded}")
    };
    let picked = key.clone();
    let (tasks, made) = app
        .call(move |db, actor| {
            work::create_plan_for_run(
                db,
                actor,
                &b.channel,
                &b.tasks,
                Some(&picked),
                b.run.as_deref(),
            )
        })
        .await?;
    let mut resp = created(location, json!({"key":key,"tasks":tasks}));
    if !made {
        *resp.status_mut() = StatusCode::OK;
    }
    Ok(resp)
}
#[utoipa::path(get, path="/task-batches/{key}", params(("key" = String, Path)), tag="task-batches")]
async fn batch(State(app): State<App>, Path(key): Path<String>) -> Data {
    lookup_batch(app, key).await
}
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
struct BatchKey {
    key: String,
}
#[utoipa::path(get, path = "/task-batches", params(BatchKey), tag = "task-batches")]
async fn batch_query(State(app): State<App>, Query(q): Query<BatchKey>) -> Data {
    lookup_batch(app, q.key).await
}
async fn lookup_batch(app: App, key: String) -> Data {
    let lookup = key.clone();
    Ok(Json(
        json!({"key":key,"tasks":app.call(move|db,actor|work::lookup_plan(db,actor,&lookup)).await?}),
    ))
}
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct NewTask {
    #[serde(default)]
    no_sync: bool,
    channel: Option<String>,
    text: Option<String>,
    #[schema(minimum = 1)]
    thread_id: Option<i64>,
    assignee: Option<String>,
    #[serde(default)]
    needs: Vec<i64>,
    #[serde(default)]
    #[schema(max_items = 10)]
    attachments: Vec<FileInput>,
    #[serde(default)]
    #[schema(max_items = 10)]
    attachment_ids: Vec<i64>,
    #[serde(default)]
    claim: bool,
    lease_seconds: Option<i64>,
    token: Option<String>,
}
#[utoipa::path(post, path = "/tasks", tag = "tasks")]
async fn create_task(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<NewTask>,
) -> ApiResult<Response> {
    let key = idempotency(&headers)?;
    let (t, made) = app
        .call(move |b, actor| {
            let opts = PostOptions {
                no_sync:body.no_sync,
                key,
                files: decode_files(body.attachments)?,
                attachment_ids:body.attachment_ids,
                ..Default::default()
            };
            let target = match (
                body.thread_id,
                body.channel.as_deref(),
                body.text.as_deref(),
            ) {
                (Some(id), None, None) if !opts.no_sync && opts.key.is_none() && opts.files.is_empty() && opts.attachment_ids.is_empty() => {
                    TaskTarget::Existing(id)
                }
                (None, Some(channel), text) => TaskTarget::New {
                    channel,
                    text:text.unwrap_or_default(),
                    options: &opts,
                },
                _ => anyhow::bail!(
                    "provide a channel and message, or thread_id without message fields or Idempotency-Key"
                ),
            };
            anyhow::ensure!(body.claim || (body.token.is_none() && body.lease_seconds.is_none()), "lease and token require claim:true");
            let proof=body.token.unwrap_or_else(||uuid::Uuid::new_v4().to_string());
            work::create_task_and_claim(b, actor, target, &body.needs, body.assignee.as_deref(),body.claim.then_some(work::ClaimRequest {lease_seconds:body.lease_seconds.unwrap_or(900),token:&proof}))
        })
        .await?;
    let mut response = created(format!("/v1/tasks/{}", t.id), json!(t));
    if !made {
        *response.status_mut() = StatusCode::OK;
    }
    Ok(response)
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(tag = "state", rename_all = "lowercase", deny_unknown_fields)]
enum StateChange {
    Completed {
        epoch: i64,
        token: String,
        result: Option<String>,
    },
    Failed {
        epoch: i64,
        token: String,
        reason: String,
    },
    Blocked {
        epoch: Option<i64>,
        token: Option<String>,
        reason: String,
        kind: Option<String>,
    },
    Canceled {
        reason: String,
    },
    Ready {},
}
#[utoipa::path(patch, path="/tasks/{id}", params(("id" = i64, Path)), tag="tasks")]
async fn update_task(
    State(app): State<App>,
    Path(id): Path<i64>,
    Json(change): Json<StateChange>,
) -> Data {
    let t = app
        .call(move |b, a| {
            let (epoch, token, op) = match &change {
                StateChange::Completed {
                    epoch,
                    token,
                    result,
                } => (
                    Some(*epoch),
                    Some(token.as_str()),
                    TaskAction::Complete(result.as_deref()),
                ),
                StateChange::Failed {
                    epoch,
                    token,
                    reason,
                } => (Some(*epoch), Some(token.as_str()), TaskAction::Fail(reason)),
                StateChange::Blocked {
                    epoch,
                    token,
                    reason,
                    kind,
                } => (
                    *epoch,
                    token.as_deref(),
                    kind.as_deref().map_or(TaskAction::Block(reason), |kind| {
                        TaskAction::BlockWithKind(reason, kind)
                    }),
                ),
                StateChange::Canceled { reason } => (None, None, TaskAction::Cancel(reason)),
                StateChange::Ready {} => (None, None, TaskAction::Resume),
            };
            work::change_task(b, id, a, epoch, token, op)
        })
        .await?;
    Ok(Json(json!(t)))
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct Assignment {
    // Explicit null clears the assignee; a missing field is not an assignment.
    #[serde(deserialize_with = "Deserialize::deserialize")]
    assignee: Option<String>,
}
#[utoipa::path(put, path="/tasks/{id}/assignment", params(("id" = i64, Path)), tag="tasks")]
async fn assignment(
    State(app): State<App>,
    Path(id): Path<i64>,
    Json(body): Json<Assignment>,
) -> Data {
    let who = body.assignee;
    Ok(Json(json!(
        app.call(move |b, a| work::change_task(
            b,
            id,
            a,
            None,
            None,
            TaskAction::Assign(who.as_deref())
        ))
        .await?
    )))
}
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct Needs {
    needs: Vec<i64>,
}
#[utoipa::path(put, path="/tasks/{id}/dependencies", params(("id" = i64, Path)), tag="tasks")]
async fn dependencies(
    State(app): State<App>,
    Path(id): Path<i64>,
    Json(body): Json<Needs>,
) -> Data {
    Ok(Json(json!(
        app.call(move |b, a| work::change_task(
            b,
            id,
            a,
            None,
            None,
            TaskAction::Needs(&body.needs)
        ))
        .await?
    )))
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct Lease {
    lease_seconds: Option<i64>,
}
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct TaskClaim {
    lease_seconds: Option<i64>,
    token: Option<String>,
}
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct RecoverClaim {
    token: String,
}
#[utoipa::path(post, path = "/task-claims/recover", tag = "task-claims")]
async fn recover(State(app): State<App>, Json(body): Json<RecoverClaim>) -> Data {
    Ok(Json(json!(
        app.call(move |b, a| work::recover_claim(b, a, &body.token))
            .await?
    )))
}

fn quoted_header(headers: &HeaderMap) -> ApiResult<Option<&str>> {
    let Some(v) = headers.get(header::IF_MATCH) else {
        return Ok(None);
    };
    let t = v
        .to_str()
        .ok()
        .and_then(|s| s.strip_prefix('"'))
        .and_then(|s| s.strip_suffix('"'))
        .filter(|s| !s.is_empty() && !s.contains('"'))
        .ok_or_else(|| invalid("If-Match expects a single quoted ETag"))?;
    Ok(Some(t))
}
fn etag(value: &str) -> ApiResult<HeaderValue> {
    HeaderValue::from_str(&format!("\"{value}\""))
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "invalid receipt"))
}
fn conditional_failure(mut err: ApiError) -> ApiError {
    if matches!(err.code, "claim_lost" | "delivery_conflict" | "no_match") {
        err.status = StatusCode::PRECONDITION_FAILED;
    }
    err
}

fn claimed((t, created_new): (Task, bool)) -> ApiResult<Response> {
    let token = t
        .claim_token
        .as_deref()
        .ok_or_else(|| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "missing claim receipt"))?;
    let tag = etag(&format!("{}:{token}", t.epoch))?;
    let mut resp = created(format!("/v1/tasks/{}/claim", t.id), json!(t));
    if !created_new {
        *resp.status_mut() = StatusCode::OK;
    }
    resp.headers_mut().insert(header::ETAG, tag);
    Ok(resp)
}
#[utoipa::path(post, path="/tasks/{id}/claim", params(("id" = i64, Path)), tag="tasks")]
async fn claim(
    State(app): State<App>,
    Path(id): Path<i64>,
    Json(body): Json<TaskClaim>,
) -> ApiResult<Response> {
    claimed(
        app.call(move |b, a| {
            let token = body
                .token
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            work::claim_in_assignment(
                b,
                a,
                id,
                work::ClaimRequest {
                    lease_seconds: body.lease_seconds.unwrap_or(900),
                    token: &token,
                },
                None,
            )
        })
        .await?,
    )
}
#[utoipa::path(get, path="/tasks/{id}/claim", params(("id" = i64, Path)), tag="tasks")]
async fn get_claim(State(app): State<App>, Path(id): Path<i64>) -> Data {
    let t = app.call(move |b, _| work::get_task(b, id)).await?;
    if t.state != "claimed" {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "no active claim"));
    }
    Ok(Json(
        json!({"id":id,"epoch":t.epoch,"holder":t.holder,"lease_until":t.lease_until}),
    ))
}
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct Renew {
    epoch: i64,
    token: String,
    lease_seconds: Option<i64>,
}
#[utoipa::path(patch, path="/tasks/{id}/claim", params(("id" = i64, Path)), tag="tasks")]
async fn renew(State(app): State<App>, Path(id): Path<i64>, Json(body): Json<Renew>) -> Data {
    Ok(Json(json!(
        app.call(move |b, a| work::change_task(
            b,
            id,
            a,
            Some(body.epoch),
            Some(&body.token),
            TaskAction::Renew(body.lease_seconds.unwrap_or(900))
        ))
        .await?
    )))
}
#[utoipa::path(delete, path="/tasks/{id}/claim", params(("id" = i64, Path)), tag="tasks")]
async fn release(
    State(app): State<App>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> ApiResult<StatusCode> {
    let tag = quoted_header(&headers)?.ok_or_else(|| {
        ApiError::new(
            StatusCode::PRECONDITION_REQUIRED,
            "release requires the claim ETag in If-Match",
        )
    })?;
    let (epoch, token) = tag
        .split_once(':')
        .ok_or_else(|| invalid("invalid claim ETag"))?;
    let epoch: i64 = epoch.parse().map_err(|_| invalid("invalid claim epoch"))?;
    let token = token.to_owned();
    app.call(move |b, a| {
        work::change_task(b, id, a, Some(epoch), Some(&token), TaskAction::Release)
    })
    .await
    .map_err(conditional_failure)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Default, Deserialize, utoipa::IntoParams)]
#[serde(default, deny_unknown_fields)]
struct WatchPage {
    after: String,
    limit: Option<usize>,
}
#[utoipa::path(get, path = "/watchers", params(WatchPage), tag = "watchers")]
async fn watchers(State(app): State<App>, Query(q): Query<WatchPage>) -> Data {
    let n = limit(q.limit)?;
    let (cursor, mut rows) = app
        .call(move |b, _| work::watchers_page(b, &q.after, n + 1))
        .await?;
    let next = continuation(&mut rows, n, |w| w.name.clone());
    Ok(Json(
        json!({"cursor":cursor,"next_after":next,"watchers":rows}),
    ))
}
#[utoipa::path(get, path="/watchers/{name}", params(("name" = String, Path)), tag="watchers")]
async fn get_watcher(State(app): State<App>, Path(name): Path<String>) -> Data {
    Ok(Json(json!(
        app.call(move |b, _| work::get_watcher(b, &name)).await?
    )))
}
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct WatchSet {
    watchers: Vec<work::WatchSpec>,
}
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct PullSet {
    limit: Option<usize>,
    lease_seconds: Option<i64>,
    #[schema(maximum = 60)]
    wait_seconds: Option<u64>,
}
#[utoipa::path(post, path = "/watcher-matches", tag = "watcher-matches")]
async fn matches(State(app): State<App>, Json(body): Json<PullSet>) -> Data {
    use std::time::{Duration, Instant};
    let wait = body.wait_seconds.unwrap_or(0);
    if wait > 60 {
        return Err(invalid("wait_seconds must be 0..60"));
    }
    let end = Instant::now() + Duration::from_secs(wait);
    let limit = body.limit.unwrap_or(10);
    let lease = body.lease_seconds.unwrap_or(300);
    loop {
        let found = app
            .call(move |b, _| work::lease_matches(b, limit, lease))
            .await?;
        if !found.is_empty() || Instant::now() >= end {
            return Ok(Json(json!({"matches":found})));
        }
        tokio::time::sleep(
            Duration::from_millis(100).min(end.saturating_duration_since(Instant::now())),
        )
        .await;
    }
}
#[utoipa::path(put, path = "/watchers", tag = "watchers")]
async fn replace_watchers(State(app): State<App>, Json(body): Json<WatchSet>) -> Data {
    Ok(Json(
        json!({"watchers":app.call(move|b,a|work::replace_watchers(b,a,&body.watchers)).await?}),
    ))
}
#[utoipa::path(post, path = "/watchers", tag = "watchers")]
async fn watcher(State(app): State<App>, Json(body): Json<work::WatchSpec>) -> ApiResult<Response> {
    let w = app
        .call(move |b, a| work::add_watcher_spec(b, a, body))
        .await?;
    Ok(created(format!("/v1/watchers/{}", w.name), json!(w)))
}
#[utoipa::path(delete, path="/watchers/{name}", params(("name" = String, Path)), tag="watchers")]
async fn remove_watcher(State(app): State<App>, Path(name): Path<String>) -> ApiResult<StatusCode> {
    app.call(move |b, a| work::remove_watcher_as(b, a, &name))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(get, path="/watchers/{name}/next", params(("name" = String, Path)), tag="watchers")]
async fn peek(State(app): State<App>, Path(name): Path<String>) -> ApiResult<Response> {
    Ok(
        match app.call(move |b, _| work::next_event(b, &name)).await? {
            Some(event) => Json(event).into_response(),
            None => StatusCode::NO_CONTENT.into_response(),
        },
    )
}
#[utoipa::path(post, path="/watchers/{name}/deliveries", params(("name" = String, Path)), tag="watchers")]
async fn deliver(
    State(app): State<App>,
    Path(name): Path<String>,
    Json(body): Json<Lease>,
) -> ApiResult<Response> {
    let d = app
        .call(move |b, _| work::lease_event(b, &name, body.lease_seconds.unwrap_or(30)))
        .await?;
    let Some(d) = d else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    let tag = etag(&d.receipt)?;
    let mut response = Json(d).into_response();
    response.headers_mut().insert(header::ETAG, tag);
    Ok(response)
}
#[derive(Default, Deserialize, utoipa::ToSchema)]
#[serde(default, deny_unknown_fields)]
struct WatcherAck {
    skip: bool,
    error: Option<String>,
    #[schema(default = false)]
    if_pending: bool,
}
#[utoipa::path(put, path="/watchers/{name}/acknowledgements/{cursor}", request_body=Option<WatcherAck>, operation_id="work_ack", params(("name" = String, Path), ("cursor" = i64, Path),("If-Match"=Option<String>,Header,description="Quoted ETag for a leased delivery")), responses((status=204,description="Settled or replayed"),(status=412,description="Stale or conflicting receipt",body=crate::Problem,content_type="application/problem+json"),(status=428,description="Missing If-Match for if_pending",body=crate::Problem,content_type="application/problem+json")), tag="watchers")]
async fn ack(
    State(app): State<App>,
    Path((name, cursor)): Path<(String, i64)>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<StatusCode> {
    let input = if body.is_empty() {
        WatcherAck::default()
    } else {
        let is_json = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(';').next())
            .is_some_and(|s| s.trim().eq_ignore_ascii_case("application/json"));
        if !is_json {
            return Err(ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "expected JSON acknowledgement",
            ));
        }
        serde_json::from_slice(&body).map_err(|_| invalid("invalid acknowledgement JSON"))?
    };
    if input.skip && input.error.is_some() {
        return Err(invalid("provide skip or error, not both"));
    }
    let receipt = quoted_header(&headers)?.map(str::to_owned);
    if input.if_pending && receipt.is_none() {
        return Err(ApiError::new(
            StatusCode::PRECONDITION_REQUIRED,
            "if_pending requires the delivery ETag in If-Match",
        ));
    }
    let conditional = receipt.is_some();
    app.call(move |b, _| {
        let outcome = if let Some(error) = input.error.as_deref() {
            work::DeliveryOutcome::Error(error)
        } else if input.skip {
            work::DeliveryOutcome::Skip
        } else {
            work::DeliveryOutcome::Ack
        };
        work::settle_delivery_if(
            b,
            &name,
            cursor,
            receipt.as_deref(),
            outcome,
            input.if_pending,
        )
    })
    .await
    .map_err(|e| {
        if conditional {
            conditional_failure(e)
        } else {
            e
        }
    })?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(patch, path="/watchers/{name}/deliveries/{cursor}", params(("name" = String, Path), ("cursor" = i64, Path)), tag="watchers")]
async fn renew_delivery(
    State(app): State<App>,
    Path((name, cursor)): Path<(String, i64)>,
    headers: HeaderMap,
    Json(body): Json<Lease>,
) -> Data {
    let proof = quoted_header(&headers)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::PRECONDITION_REQUIRED,
                "renewal requires the delivery ETag",
            )
        })?
        .to_owned();
    let until = app
        .call(move |b, _| {
            work::renew_delivery(b, &name, cursor, &proof, body.lease_seconds.unwrap_or(30))
        })
        .await
        .map_err(conditional_failure)?;
    Ok(Json(json!({"lease_until":until})))
}

#[derive(Default, Deserialize, utoipa::IntoParams)]
#[serde(default, deny_unknown_fields)]
struct FailurePage {
    after: Option<i64>,
    limit: Option<usize>,
}
#[utoipa::path(get, path="/watchers/{name}/failures", params(("name" = String, Path), FailurePage), tag="watchers")]
async fn failures(
    State(app): State<App>,
    Path(name): Path<String>,
    Query(q): Query<FailurePage>,
) -> Data {
    let (at, n) = (after(q.after)?, limit(q.limit)?);
    let (cursor, mut rows) = app
        .call(move |b, _| work::watcher_failures(b, &name, at, n + 1))
        .await?;
    let next = continuation(&mut rows, n, |f| f.cursor);
    Ok(Json(
        json!({"cursor":cursor,"next_after":next,"failures":rows}),
    ))
}

#[cfg(test)]
mod tests;
