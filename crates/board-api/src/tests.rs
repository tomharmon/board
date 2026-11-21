use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Method, Request},
};
use tower::ServiceExt;

fn api() -> Router {
    routes(App {
        db: Arc::new(Mutex::new(Board::memory().unwrap())),
        actor: "ada/helper".into(),
        token: "test-local-token".into(),
    })
}
async fn send(
    app: &Router,
    method: Method,
    path: &str,
    data: Value,
    key: Option<&str>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", "Bearer test-local-token")
        .header("content-type", "application/json");
    if let Some(key) = key {
        req = req.header("idempotency-key", key);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::from(data.to_string())).unwrap())
        .await
        .unwrap();
    let (parts, body) = resp.into_parts();
    (
        parts.status,
        parts.headers,
        to_bytes(body, 1024 * 1024).await.unwrap().to_vec(),
    )
}
fn parse(buf: &[u8]) -> Value {
    serde_json::from_slice(buf).unwrap()
}

#[tokio::test]
async fn advertised_resources_exist_and_sources_round_trip_offline() {
    use Method as M;
    let app = api();
    let (_, _, spec) = send(&app, M::GET, "/v1/openapi.json", Value::Null, None).await;
    for (path, methods) in parse(&spec)["paths"].as_object().unwrap() {
        let concrete = path
            .split('/')
            .map(|part| if part.starts_with('{') { "1" } else { part })
            .collect::<Vec<_>>()
            .join("/");
        let uri = if concrete == "/" {
            "/v1".into()
        } else {
            format!("/v1{concrete}")
        };
        let (s, headers, _) = send(&app, M::OPTIONS, &uri, Value::Null, None).await;
        assert_eq!(s, 405, "missing {uri}");
        let allow = headers["allow"].to_str().unwrap().to_ascii_lowercase();
        for m in methods
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| *k != "parameters")
        {
            assert!(
                allow.split(',').any(|v| v.trim() == m),
                "missing {m} on {uri}: {allow}"
            );
        }
    }
    let (s, _, me) = send(&app, M::GET, "/v1/me", Value::Null, None).await;
    assert_eq!(s, 200);
    assert_eq!(parse(&me)["actor"], "ada/helper");
    let (s, h, _) = send(
        &app,
        M::POST,
        "/v1/sources",
        json!({"name":"bugs","remote":"issues","provider":"local","channel":"triage"}),
        None,
    )
    .await;
    assert_eq!(s, 201);
    assert_eq!(h["location"], "/v1/sources/bugs");
    let e = json!({"key":"k","import":{"thread":"T","external":"r","author":"user","change":{"op":"post","text":"inspect me"}},"raw":{"provider":17}});
    let (s, h, _) = send(&app, M::POST, "/v1/sources/bugs/inbox", e.clone(), None).await;
    assert_eq!(s, 202);
    let job = h["location"].to_str().unwrap();
    let (s, _, body) = send(
        &app,
        M::POST,
        "/v1/sources/bugs/deliveries",
        json!({"direction":"in"}),
        None,
    )
    .await;
    assert_eq!(s, 201);
    let receipt = parse(&body)["receipt"].clone();
    let (s, _, body) = send(
        &app,
        M::PUT,
        &format!("{job}/receipt"),
        json!({"receipt":receipt}),
        None,
    )
    .await;
    assert_eq!(s, 200);
    let m = parse(&body);
    assert_eq!(m["text"], "inspect me");
    let (s, _, body) = send(&app, M::POST, "/v1/sources/bugs/imports", e, None).await;
    assert_eq!(s, 200);
    assert_eq!(parse(&body)["id"], m["id"]);
    let (s, _, body) = send(
        &app,
        M::GET,
        "/v1/sources/bugs/links?thread=T&external=r",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(parse(&body)["id"], m["id"]);
    let typing = format!("/v1/threads/{}/typing", m["id"]);
    assert_eq!(
        send(&app, M::PUT, &typing, json!({"status":"Reviewing"}), None)
            .await
            .0,
        204
    );
    let (_, _, body) = send(&app, M::GET, &typing, Value::Null, None).await;
    assert_eq!(parse(&body)["typing"][0]["status"], "Reviewing");
    assert_eq!(
        send(&app, M::DELETE, &typing, Value::Null, None).await.0,
        204
    );
}

#[tokio::test]
async fn compound_label_filters_and_live_sse_recheck_revocation() {
    use Method as M;
    use futures_util::StreamExt;
    let app = api();
    for (text, l) in [
        ("release", json!({"ready":"","stage":"ok"})),
        ("other", json!({"ready":"yes","stage":"ok"})),
    ] {
        assert_eq!(
            send(
                &app,
                M::POST,
                "/v1/messages",
                json!({"channel":"ops","text":text,"labels":l}),
                None
            )
            .await
            .0,
            201
        );
    }
    let (_, _, body) = send(
        &app,
        M::GET,
        "/v1/messages?label=ready%3D&label=stage%3Dok%7Cgo",
        Value::Null,
        None,
    )
    .await;
    let docs = parse(&body);
    let hits = docs["messages"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["text"], "release");
    let (s, _, _) = send(
        &app,
        M::POST,
        "/v1/grants",
        json!({"subject":"sam","can":{"ops":["read"]}}),
        None,
    )
    .await;
    assert_eq!(s, 201);
    let (s, h, key) = send(
        &app,
        M::POST,
        "/v1/credentials",
        json!({"actor":"sam"}),
        None,
    )
    .await;
    assert_eq!(s, 201);
    let key = parse(&key);
    let req = Request::builder()
        .uri("/v1/events?channel=ops")
        .header(
            "authorization",
            format!("Bearer {}", key["token"].as_str().unwrap()),
        )
        .header("accept", "text/event-stream")
        .header("last-event-id", "0")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    let mut stream = resp.into_body().into_data_stream();
    let chunk = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        std::str::from_utf8(&chunk)
            .unwrap()
            .contains("message.posted")
    );
    assert_eq!(
        send(
            &app,
            M::DELETE,
            h["location"].to_str().unwrap(),
            Value::Null,
            None
        )
        .await
        .0,
        204
    );
    let chunk = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        std::str::from_utf8(&chunk)
            .unwrap()
            .contains("invalid_credential")
    );
    assert!(stream.next().await.is_none());
}

#[test]
fn resources_have_locations_statuses_and_replay_safe_writes() {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        use Method as M;
        let app=api();
        let (s,h,_)=send(&app,M::POST,"/v1/channels",json!({"name":"ops/log"}),None).await;
        assert_eq!(s,201);
        let (s,_,body)=send(&app,M::GET,h["location"].to_str().unwrap(),Value::Null,None).await;
        assert_eq!(s,200);assert_eq!(parse(&body)["threads"],0);
        let input=json!({"channel":"ops/log","text":"Inspection","attachments":[{"name":"proof.bin","data_base64":"AP8="}]});
        let (s,h,body)=send(&app,M::POST,"/v1/messages",input.clone(),Some("write-once")).await;
        assert_eq!(s,201);
        let message=parse(&body);
        assert_eq!(message["actor"],"ada/helper");
        let url=h["location"].to_str().unwrap();
        let (_,_,read)=send(&app,M::GET,url,Value::Null,None).await;
        assert_eq!(message,parse(&read));
        let (s,_,again)=send(&app,M::POST,"/v1/messages",input,Some("write-once")).await;
        assert_eq!(s,200);assert_eq!(message,parse(&again));
        let (s,_,again)=send(&app,M::POST,"/v1/messages",json!({"channel":"ops/log","text":"Recomputed"}),Some("write-once")).await;
        assert_eq!(s,200);assert_eq!(message,parse(&again));
        let (s,_,receipt)=send(&app,M::GET,"/v1/posts/write-once",Value::Null,None).await;
        assert_eq!(s,200);assert_eq!(message,parse(&receipt));
        let (s,_,err)=send(&app,M::POST,"/v1/messages",json!({"channel":"ops/another","text":"Other"}),Some("write-once")).await;
        assert_eq!(s,409);assert_eq!(parse(&err)["code"],"duplicate_id");
        let file=message["attachments"][0]["id"].as_i64().unwrap();
        let (s,h,bytes)=send(&app,M::GET,&format!("/v1/attachments/{file}"),Value::Null,None).await;
        assert_eq!(s,200);assert_eq!(h[header::CONTENT_TYPE],"application/octet-stream");assert_eq!(bytes,[0,255]);
        let rev=message["revision"].clone();
        let (s,_,_)=send(&app,M::PATCH,url,json!({"text":"Checked","if_revision":rev}),None).await;
        assert_eq!(s,200);
        let (s,_,err)=send(&app,M::DELETE,&format!("{url}?if_revision={rev}"),Value::Null,None).await;
        assert_eq!(s,409);assert_eq!(parse(&err)["code"],"revision_conflict");
        let (s,_,_)=send(&app,M::DELETE,url,Value::Null,None).await;
        assert_eq!(s,204);
        let (_,_,read)=send(&app,M::GET,url,Value::Null,None).await;
        assert_eq!(parse(&read)["deleted"],true);
    });
}

#[test]
fn errors_and_access_boundary_are_consistent_even_before_handlers() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let app = api();
            for (verb, path, data, auth, origin, expected) in [
                ("GET", "/v1/channels", "", false, false, 401),
                ("GET", "/v1/channels", "", true, true, 403),
                ("POST", "/v1/messages", "{", true, false, 400),
                (
                    "POST",
                    "/v1/messages",
                    r#"{"channel":"ok","text":"hi","actor":"another"}"#,
                    true,
                    false,
                    422,
                ),
                ("GET", "/v1/messages/not-an-id", "", true, false, 400),
                ("GET", "/v1/events?after=-1", "", true, false, 400),
                ("POST", "/v1/events", "{}", true, false, 405),
                ("GET", "/v1/no-such-resource", "", true, false, 404),
            ] {
                let mut req = Request::builder()
                    .method(verb)
                    .uri(path)
                    .header("content-type", "application/json");
                if auth {
                    req = req.header(header::AUTHORIZATION, "Bearer test-local-token");
                }
                if origin {
                    req = req.header(header::ORIGIN, "http://localhost");
                }
                let resp = app
                    .clone()
                    .oneshot(req.body(Body::from(data)).unwrap())
                    .await
                    .unwrap();
                assert_eq!(resp.status(), expected, "{path}");
                assert_eq!(
                    resp.headers()[header::CONTENT_TYPE],
                    "application/problem+json"
                );
                assert_eq!(resp.headers()[header::CACHE_CONTROL], "no-store");
                let problem = parse(&to_bytes(resp.into_body(), 16384).await.unwrap());
                assert_eq!(problem["status"], expected);
                assert!(problem["code"].is_string() && problem["detail"].is_string());
            }
        });
}

#[test]
fn pending_event_poll_releases_database_and_resumes_from_last_delivery() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let app = api();
            let reader = app.clone();
            let waiting = tokio::spawn(async move {
                send(
                    &reader,
                    Method::GET,
                    "/v1/events?channel=ops%2F&after=0&limit=1&wait_ms=3000",
                    Value::Null,
                    None,
                )
                .await
            });
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            for (channel, text) in [
                ("other", "noise"),
                ("ops/bench", "one"),
                ("ops/bench", "two"),
            ] {
                let (s, _, _) = send(
                    &app,
                    Method::POST,
                    "/v1/messages",
                    json!({"channel":channel,"text":text}),
                    None,
                )
                .await;
                assert_eq!(s, 201);
            }
            let (s, _, body) = waiting.await.unwrap();
            let first = parse(&body);
            assert_eq!(s, 200);
            assert_eq!(first["events"][0]["message"]["text"], "one");
            let path = format!(
                "/v1/events?channel=ops%2F&limit=1&after={}",
                first["cursor"]
            );
            let (_, _, body) = send(&app, Method::GET, &path, Value::Null, None).await;
            let second = parse(&body);
            assert_eq!(second["events"][0]["message"]["text"], "two");
            assert_eq!(second["cursor"], second["events"][0]["cursor"]);
        });
}
