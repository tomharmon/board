use super::*;
use axum::extract::Path;
use serde::Deserialize;

pub(super) fn routes() -> OpenApiRouter<App> {
    OpenApiRouter::new()
        .routes(utoipa_axum::routes!(root))
        .routes(utoipa_axum::routes!(discovery))
        .routes(utoipa_axum::routes!(me))
        .routes(utoipa_axum::routes!(openapi))
        .routes(utoipa_axum::routes!(active, typing, clear))
}
pub fn document() -> Value {
    json!({"name":"Board","version":env!("CARGO_PKG_VERSION"),"api":"v1","storage":"sqlite","transport":"loopback",
        "links":{"self":"/v1/discovery","openapi":"/v1/openapi.json","me":"/v1/me","channels":"/v1/channels","threads":"/v1/threads","events":"/v1/events","sources":"/v1/sources"},
        "features":["threads","reactions","schemas","uploads","tasks","watchers","channel-grants","sources","inbox","outbox","sse","local-typing","local-only-messages","channel-mirrors"],
        "providers":["local","github","slack","linear"],"limits":{"page":500,"message_characters":10000,"inline_file_bytes":board_local::MAX_ATTACHMENT_BYTES}})
}
#[utoipa::path(get, path = "/", tag = "discovery")]
async fn root() -> Json<Value> {
    discovery().await
}
#[utoipa::path(
    get,
    path = "/discovery",
    operation_id = "catalog_discovery",
    tag = "discovery"
)]
async fn discovery() -> Json<Value> {
    Json(document())
}
#[utoipa::path(get, path = "/me", tag = "me")]
async fn me(State(a): State<App>) -> ApiResult<Json<Value>> {
    Ok(Json(
        a.call(|b, who| {
            let mut me = json!(b.viewer(who)?);
            me["transport"] = json!("loopback");
            Ok(me)
        })
        .await?,
    ))
}
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
struct Status {
    status: Option<String>,
    seconds: Option<i64>,
}
#[utoipa::path(get, path="/threads/{id}/typing", params(("id" = i64, Path)), tag="threads")]
async fn active(State(a): State<App>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({"typing":a.call(move|b,_|b.typing(id)).await?})))
}
#[utoipa::path(put, path="/threads/{id}/typing", params(("id" = i64, Path)), tag="threads")]
async fn typing(
    State(a): State<App>,
    Path(id): Path<i64>,
    Json(i): Json<Status>,
) -> ApiResult<StatusCode> {
    a.call(move |b, who| {
        b.set_typing(
            id,
            who,
            Some(i.status.as_deref().unwrap_or("Typing")),
            i.seconds.unwrap_or(15),
        )
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(delete, path="/threads/{id}/typing", params(("id" = i64, Path)), tag="threads")]
async fn clear(State(a): State<App>, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    a.call(move |b, who| b.set_typing(id, who, None, 1)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/openapi.json", tag = "openapi.json")]
async fn openapi(axum::Extension(spec): axum::Extension<Arc<Value>>) -> Json<Value> {
    Json((*spec).clone())
}

pub(super) fn spec(contract: utoipa::openapi::OpenApi) -> Value {
    let mut doc = json!(contract);
    // Axum's catch-all marker is not part of an OpenAPI parameter name.
    let paths = doc["paths"].as_object_mut().unwrap();
    let channel = paths.remove("/channels/{*name}").expect("channel route");
    paths.insert("/channels/{name}".into(), channel);
    for methods in paths.values_mut() {
        for (method, op) in methods.as_object_mut().unwrap() {
            if method == "parameters" {
                continue;
            }
            if op["responses"].as_object().is_none_or(|r| r.is_empty()) {
                op["responses"]["2XX"] = json!({"description":"Successful local operation"});
            }
            op["responses"]["default"] = json!({"description":"Rejected request","content":{"application/problem+json":{"schema":{"$ref":"#/components/schemas/Problem"}}}});
        }
    }
    doc["info"] = json!({"title":"Board local REST API","version":env!("CARGO_PKG_VERSION")});
    doc["servers"] = json!([{"url":"/v1"}]);
    doc["security"] = json!([{"localBearer":[]}]);
    doc["components"]["securitySchemes"] = json!({"localBearer":{"type":"http","scheme":"bearer"}});
    // These destinations share fields but must be mutually exclusive.
    for (model, first, second) in [
        ("NewMessage", "channel", "reply_to"),
        ("NewTask", "channel", "thread_id"),
        ("PlanItem", "text", "thread_id"),
    ] {
        doc["components"]["schemas"][model]["oneOf"] = json!([
            {"required":[first],"properties":{(first):{"type":"string"}}},
            {"required":[second],"properties":{(second):{"type":"integer","minimum":1}}}
        ]);
    }
    doc
}
