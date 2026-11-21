use super::*;
use axum::extract::Path;
use board_local::policy::Rule;
use serde::Deserialize;
pub(super) fn routes() -> Router<App> {
    Router::new()
        .route("/rules", get(list))
        .route("/rules/{name}", get(one).put(install).delete(remove))
}
async fn list(State(a): State<App>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({"rules":a.call(|b,_|b.write_rules()).await?})))
}
async fn one(State(a): State<App>, Path(name): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(
        a.call(move |b, _| b
            .write_rules()?
            .into_iter()
            .find(|r| r.name == name)
            .ok_or_else(|| board_local::errors::Kind::Missing
                .error("rule_not_found", "unknown rule")
                .into()))
            .await?
    )))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    #[serde(default)]
    priority: i64,
    script: String,
}
async fn install(
    State(a): State<App>,
    Path(name): Path<String>,
    Json(i): Json<Body>,
) -> ApiResult<Response> {
    let location = format!("/v1/rules/{name}");
    let fresh = a
        .call(move |b, u| {
            b.set_write_rule(
                u,
                &Rule {
                    name,
                    priority: i.priority,
                    script: i.script,
                },
            )
        })
        .await?;
    Ok(if fresh {
        created(location, json!({"created":true}))
    } else {
        StatusCode::NO_CONTENT.into_response()
    })
}
async fn remove(State(a): State<App>, Path(name): Path<String>) -> ApiResult<StatusCode> {
    a.call(move |b, u| b.remove_write_rule(u, &name)).await?;
    Ok(StatusCode::NO_CONTENT)
}
