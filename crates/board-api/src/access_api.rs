use super::*;
use axum::{
    extract::{Path, Query},
    routing::post,
};
use board_local::access::{GrantInput as GrantBody, Rules};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchBody {
    grants: Vec<GrantBody>,
    #[serde(default)]
    lock_me_out: bool,
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Confirm {
    lock_me_out: bool,
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PatchGrant {
    add: Vec<String>,
    remove: Vec<String>,
    can: Option<Rules>,
    lock_me_out: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Issue {
    actor: String,
    scope: Option<Rules>,
    ttl: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Explain {
    subject: String,
    channel: String,
}

pub(super) fn routes() -> Router<App> {
    Router::new()
        .route("/grants", get(grants).post(create))
        .route(
            "/grants/{id}",
            get(grant).put(update).patch(change).delete(revoke),
        )
        .route("/grant-batches", post(batch_create))
        .route("/grant-batches/{id}", get(batch_get))
        .route("/access", get(explain))
        .route("/credentials", get(credentials).post(issue))
        .route("/credentials/{id}", get(credential).delete(cancel))
}
async fn grants(State(a): State<App>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({"grants":a.call(|b,_|b.grants()).await?})))
}
async fn grant(State(a): State<App>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({"grant":a.call(move|b,_|b.grant(id)).await?})))
}
async fn create(
    State(a): State<App>,
    Query(opts): Query<Confirm>,
    Json(i): Json<GrantBody>,
) -> ApiResult<Response> {
    let g = a
        .call(move |b, who| {
            let (members, rules) = i.parts()?;
            b.set_group_grant(who, None, &members, &rules, opts.lock_me_out)
        })
        .await?;
    Ok(created(format!("/v1/grants/{}", g.id), json!({"grant":g})))
}
async fn update(
    State(a): State<App>,
    Path(id): Path<i64>,
    Query(opts): Query<Confirm>,
    Json(i): Json<GrantBody>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        json!({"grant":a.call(move|b,who|{let (members,rules)=i.parts()?;b.set_group_grant(who,Some(id),&members,&rules,opts.lock_me_out)}).await?}),
    ))
}
async fn change(
    State(a): State<App>,
    Path(id): Path<i64>,
    Json(i): Json<PatchGrant>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        json!({"grant":a.call(move |b,who|b.patch_grant(who,id,&i.add,&i.remove,i.can.as_ref(),i.lock_me_out)).await?}),
    ))
}
async fn batch_create(
    State(a): State<App>,
    headers: HeaderMap,
    Json(i): Json<BatchBody>,
) -> ApiResult<Response> {
    let key = idempotency(&headers)?;
    let (batch, made) = a
        .call(move |b, owner| {
            b.group_grant_batch(
                owner,
                &i.grants
                    .into_iter()
                    .map(GrantBody::parts)
                    .collect::<Result<Vec<_>>>()?,
                key.as_deref(),
                i.lock_me_out,
            )
        })
        .await?;
    let location = format!("/v1/grant-batches/{}", batch.id);
    let body = json!({"batch":batch,"created":made});
    Ok(if made {
        created(location, body)
    } else {
        Json(body).into_response()
    })
}
async fn batch_get(State(a): State<App>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    Ok(Json(
        json!({"batch":a.call(move |b,_| b.grant_batch(id)).await?}),
    ))
}
async fn revoke(
    State(a): State<App>,
    Path(id): Path<i64>,
    Query(opts): Query<Confirm>,
) -> ApiResult<StatusCode> {
    a.call(move |b, who| b.revoke_grant_with_outcome(who, id, opts.lock_me_out))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn explain(State(a): State<App>, Query(i): Query<Explain>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(
        a.call(move |b, _| b.explain_decision(&i.subject, &i.channel))
            .await?
    )))
}
async fn credentials(State(a): State<App>) -> ApiResult<Json<Value>> {
    Ok(Json(
        json!({"credentials":a.call(|b,_|b.credentials()).await?}),
    ))
}
async fn credential(State(a): State<App>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(
        json!({"credential":a.call(move|b,_|b.credential(&id)).await?}),
    ))
}
async fn issue(State(a): State<App>, Json(i): Json<Issue>) -> ApiResult<Response> {
    let made = a
        .call(move |b, who| b.issue_credential(who, &i.actor, i.scope.as_ref(), i.ttl))
        .await?;
    Ok(created(
        format!("/v1/credentials/{}", made.credential.id),
        json!(made),
    ))
}
async fn cancel(State(a): State<App>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    a.call(move |b, who| b.revoke_credential(who, &id)).await?;
    Ok(StatusCode::NO_CONTENT)
}
