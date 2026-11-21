use super::*;
use axum::extract::Path;
use serde::Deserialize;

pub(super) fn routes() -> Router<App> {
    Router::new()
        .route("/", get(discovery))
        .route("/discovery", get(discovery))
        .route("/me", get(me))
        .route("/openapi.json", get(openapi))
        .route(
            "/threads/{id}/typing",
            get(active).put(typing).delete(clear),
        )
}
pub fn document() -> Value {
    json!({"name":"Board","version":env!("CARGO_PKG_VERSION"),"api":"v1","storage":"sqlite","transport":"loopback",
        "links":{"self":"/v1/discovery","openapi":"/v1/openapi.json","me":"/v1/me","channels":"/v1/channels","threads":"/v1/threads","events":"/v1/events","sources":"/v1/sources"},
        "features":["threads","labels","reactions","schemas","uploads","tasks","schedules","watchers","channel-grants","sources","inbox","outbox","sse","local-typing","local-only-messages","channel-mirrors"],
        "providers":["local","github","slack","linear"],"limits":{"page":500,"message_characters":10000,"inline_file_bytes":board_local::MAX_ATTACHMENT_BYTES}})
}
async fn discovery() -> Json<Value> {
    Json(document())
}
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
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Status {
    status: Option<String>,
    seconds: Option<i64>,
}
async fn active(State(a): State<App>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({"typing":a.call(move|b,_|b.typing(id)).await?})))
}
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
async fn clear(State(a): State<App>, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    a.call(move |b, who| b.set_typing(id, who, None, 1)).await?;
    Ok(StatusCode::NO_CONTENT)
}

// Each entry is checked against OPTIONS/Allow from the real Axum router in the contract tests.
pub(super) const RESOURCES: &[(&str, &str)] = &[
    ("/", "get"),
    ("/discovery", "get"),
    ("/me", "get"),
    ("/openapi.json", "get"),
    ("/channels", "get post"),
    ("/channels/{name}", "get"),
    ("/actors", "get"),
    ("/profile", "put"),
    ("/threads", "get"),
    ("/threads/{id}", "get"),
    ("/threads/{id}/typing", "get put delete"),
    ("/messages", "get post"),
    ("/messages/{id}", "get patch delete"),
    ("/messages/{id}/deliveries", "get"),
    ("/messages/{id}/sync", "get"),
    ("/links", "get"),
    ("/posts/{key}", "get"),
    ("/messages/{id}/labels", "patch"),
    ("/messages/{id}/reactions/{name}", "put delete"),
    ("/attachments", "post"),
    ("/attachments/{id}", "get"),
    ("/uploads", "post"),
    ("/uploads/{key}", "get"),
    ("/uploads/{key}/parts/{offset}", "put"),
    ("/uploads/{key}/completion", "put"),
    ("/events", "get"),
    ("/schemas", "get"),
    ("/schemas/{name}/{version}", "get put"),
    ("/grants", "get post"),
    ("/grants/{id}", "get put patch delete"),
    ("/grant-batches", "post"),
    ("/grant-batches/{id}", "get"),
    ("/access", "get"),
    ("/credentials", "get post"),
    ("/credentials/{id}", "get delete"),
    ("/tasks", "get post"),
    ("/tasks/{id}", "get patch"),
    ("/tasks/{id}/assignment", "put"),
    ("/tasks/{id}/dependencies", "put"),
    ("/tasks/{id}/claim", "get post patch delete"),
    ("/task-claims", "post"),
    ("/task-claims/recover", "post"),
    ("/task-batches", "get post"),
    ("/task-batches/{key}", "get"),
    ("/schedules", "get post"),
    ("/schedules/{id}", "get delete"),
    ("/watchers", "get post put"),
    ("/watcher-matches", "post"),
    ("/watchers/{name}", "get delete"),
    ("/watchers/{name}/next", "get"),
    ("/watchers/{name}/deliveries", "post"),
    ("/watchers/{name}/deliveries/{cursor}", "patch"),
    ("/watchers/{name}/failures", "get"),
    ("/watchers/{name}/acknowledgements/{cursor}", "put"),
    ("/sources", "get post"),
    ("/sources/{name}", "get patch"),
    ("/sources/{name}/links", "get put"),
    ("/sources/{name}/thread-links", "get put"),
    ("/sources/{name}/imports", "post"),
    ("/sources/{name}/inbox", "get post"),
    ("/sources/{name}/outbox", "get"),
    ("/sources/{name}/starts", "post"),
    ("/sources/{name}/backfills", "post"),
    ("/sources/{name}/files/{id}", "get post"),
    ("/sources/{name}/copies/{id}", "get"),
    ("/sources/{name}/deliveries", "post"),
    ("/sources/{name}/deliveries/{id}", "get"),
    ("/sources/{name}/deliveries/{id}/receipt", "put"),
    ("/sources/{name}/deliveries/{id}/failure", "post"),
    ("/sources/{name}/deliveries/{id}/resolution", "put"),
];
fn spec() -> Value {
    let mut paths = serde_json::Map::new();
    for &(path, verbs) in RESOURCES {
        let path_params = path
            .split('/')
            .filter_map(|p| p.strip_prefix('{')?.strip_suffix('}'))
            .map(|p| json!({"name":p,"in":"path","required":true,"schema":{"type":"string"}}))
            .collect::<Vec<_>>();
        let mut def = json!({});
        if !path_params.is_empty() {
            def["parameters"] = json!(path_params);
        }
        for method in verbs.split_whitespace() {
            let tag = path
                .trim_matches('/')
                .split('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("discovery");
            let mut op = json!({"summary":format!("{} {path}",method.to_uppercase()),"tags":[tag],"responses":{"2XX":{"description":"Successful local operation"},"default":{"description":"Rejected request","content":{"application/problem+json":{"schema":{"$ref":"#/components/schemas/Problem"}}}}}});
            if matches!(method, "post" | "put" | "patch")
                && !path.contains("/reactions/")
                && !path.ends_with("/completion")
            {
                let binary = path == "/attachments" || path.contains("/parts/");
                op["requestBody"] = if binary {
                    json!({"content":{"application/octet-stream":{"schema":{"type":"string","format":"binary"}}}})
                } else {
                    json!({"content":{"application/json":{"schema":{"type":"object"}}}})
                };
            }
            crate::openapi::describe(&mut op, path, method);
            def[method] = op;
        }
        paths.insert(path.to_owned(), def);
    }
    json!({"openapi":"3.1.0","info":{"title":"Board local REST API","version":env!("CARGO_PKG_VERSION")},"servers":[{"url":"/v1"}],"security":[{"localBearer":[]}],"paths":paths,
    "components":{"securitySchemes":{"localBearer":{"type":"http","scheme":"bearer"}},"schemas":crate::openapi::models()}})
}
async fn openapi() -> Json<Value> {
    Json(spec())
}
