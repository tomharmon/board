use super::*;
use axum::{
    body::Bytes,
    extract::{Path, Query},
    routing::post,
};
use board_local::MAX_ATTACHMENT_BYTES;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum FileRef {
    Id(i64),
    Key(String),
}
pub(super) fn resolve(b: &Board, who: &str, refs: &[FileRef]) -> Result<Vec<i64>> {
    ensure!(
        refs.len() <= board_local::MAX_ATTACHMENTS,
        "too many attachments"
    );
    refs.iter()
        .map(|r| match r {
            FileRef::Id(id) => Ok(*id),
            FileRef::Key(key) => b.file_id(who, key),
        })
        .collect()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Simple {
    channel: String,
    name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    channel: String,
    name: String,
    content_type: Option<String>,
    size: i64,
    sha256: Option<String>,
}
pub(super) fn routes() -> Router<App> {
    Router::new()
        .route(
            "/attachments",
            post(upload).layer(DefaultBodyLimit::max(MAX_ATTACHMENT_BYTES)),
        )
        .route("/uploads", post(start))
        .route("/uploads/{key}", get(status))
        .route(
            "/uploads/{key}/parts/{offset}",
            put(part).layer(DefaultBodyLimit::max(MAX_ATTACHMENT_BYTES)),
        )
        .route("/uploads/{key}/completion", put(finish))
}
async fn upload(
    State(a): State<App>,
    Query(q): Query<Simple>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let content = headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();
    let file = a
        .call(move |b, who| b.upload(who, &q.channel, &q.name, &content, &body))
        .await?;
    Ok(created(
        format!("/v1/attachments/{}", file.key),
        json!(file),
    ))
}
async fn start(State(a): State<App>, Json(i): Json<Start>) -> ApiResult<Response> {
    let up = a
        .call(move |b, who| {
            b.start_upload(
                who,
                &i.channel,
                &i.name,
                i.content_type
                    .as_deref()
                    .unwrap_or("application/octet-stream"),
                i.size,
                i.sha256.as_deref(),
            )
        })
        .await?;
    Ok(created(format!("/v1/uploads/{}", up.key), json!(up)))
}
async fn status(State(a): State<App>, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(
        a.call(move |b, who| b.upload_status(who, &key)).await?
    )))
}
async fn part(
    State(a): State<App>,
    Path((key, offset)): Path<(String, i64)>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    Ok(Json(json!(
        a.call(move |b, who| b.upload_part(who, &key, offset, &body))
            .await?
    )))
}
async fn finish(State(a): State<App>, Path(key): Path<String>) -> ApiResult<Response> {
    let (file, new) = a.call(move |b, who| b.finish_upload(who, &key)).await?;
    Ok(if new {
        created(format!("/v1/attachments/{}", file.key), json!(file))
    } else {
        Json(json!(file)).into_response()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    async fn req(
        app: &Router,
        verb: &str,
        path: &str,
        body: impl Into<Body>,
        kind: &str,
    ) -> (u16, HeaderMap, Vec<u8>) {
        let req = Request::builder()
            .method(verb)
            .uri(path)
            .header(header::AUTHORIZATION, "Bearer api-test-secret")
            .header(header::CONTENT_TYPE, kind)
            .body(body.into())
            .unwrap();
        let (parts, body) = app.clone().oneshot(req).await.unwrap().into_parts();
        (
            parts.status.as_u16(),
            parts.headers,
            to_bytes(body, 1024 * 1024).await.unwrap().to_vec(),
        )
    }
    #[test]
    fn staged_asset_locations_and_id_reuse_work_over_resource_routes() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let app = crate::routes(App {
                    db: Arc::new(Mutex::new(Board::memory().unwrap())),
                    actor: "ann".into(),
                    token: "api-test-secret".into(),
                });
                let (code, h, _) = req(
                    &app,
                    "POST",
                    "/v1/attachments?channel=dev&name=small.txt",
                    "proof",
                    "text/plain",
                )
                .await;
                assert_eq!(code, 201);
                let (code, h, body) = req(
                    &app,
                    "GET",
                    h[header::LOCATION].to_str().unwrap(),
                    "",
                    "text/plain",
                )
                .await;
                assert_eq!(code, 200);
                assert_eq!(body, b"proof");
                assert_eq!(h[header::CONTENT_TYPE], "text/plain");
                assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
                let (code, h, _) = req(
                    &app,
                    "POST",
                    "/v1/uploads",
                    json!({"channel":"dev","name":"x.bin","size":6}).to_string(),
                    "application/json",
                )
                .await;
                assert_eq!(code, 201);
                let loc = h[header::LOCATION].to_str().unwrap();
                assert_eq!(
                    req(
                        &app,
                        "PUT",
                        &format!("{loc}/parts/0"),
                        "abc",
                        "application/octet-stream"
                    )
                    .await
                    .0,
                    200
                );
                assert_eq!(
                    req(&app, "PUT", &format!("{loc}/completion"), "", "text/plain")
                        .await
                        .0,
                    409
                );
                assert_eq!(
                    req(
                        &app,
                        "PUT",
                        &format!("{loc}/parts/3"),
                        "def",
                        "application/octet-stream"
                    )
                    .await
                    .0,
                    200
                );
                let (code, h, body) =
                    req(&app, "PUT", &format!("{loc}/completion"), "", "text/plain").await;
                assert_eq!(code, 201);
                let file: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(
                    req(&app, "PUT", &format!("{loc}/completion"), "", "text/plain")
                        .await
                        .0,
                    200
                );
                let (code, _, body) = req(
                    &app,
                    "POST",
                    "/v1/messages",
                    json!({"channel":"dev","attachment_ids":[file["key"]]}).to_string(),
                    "application/json",
                )
                .await;
                assert_eq!(code, 201);
                assert_eq!(
                    serde_json::from_slice::<Value>(&body).unwrap()["attachments"][0],
                    file
                );
                assert_eq!(
                    req(
                        &app,
                        "GET",
                        h[header::LOCATION].to_str().unwrap(),
                        "",
                        "text/plain"
                    )
                    .await
                    .2,
                    b"abcdef"
                );
            });
    }
}
