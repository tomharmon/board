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
async fn future_inbox_and_outbox_cursors_resume_at_their_own_queues() {
    use Method as M;
    let app = api();
    assert_eq!(
        send(
            &app,
            M::POST,
            "/v1/sources",
            json!({"name":"audit","channel":"desk","provider":"local","remote":"archive"}),
            None
        )
        .await
        .0,
        201
    );
    assert_eq!(
        send(
            &app,
            M::PATCH,
            "/v1/sources/audit",
            json!({"outbound":true}),
            None
        )
        .await
        .0,
        200
    );
    let inbound = |key: &str| {
        send(
            &app,
            M::POST,
            "/v1/sources/audit/inbox",
            json!({"key":key,"import":{"thread":key,"external":key,"author":"writer","change":{"op":"post","text":key}},"raw":{}}),
            None,
        )
    };
    let outbound = || {
        send(
            &app,
            M::POST,
            "/v1/messages",
            json!({"channel":"desk","text":"new issue","sync_targets":["audit"]}),
            None,
        )
    };
    assert_eq!(inbound("first").await.0, 202);
    assert_eq!(outbound().await.0, 201);
    let mut checkpoints = Vec::new();
    for queue in ["inbox", "outbox"] {
        let path = format!("/v1/sources/audit/{queue}");
        let (status, _, raw) = send(&app, M::GET, &path, Value::Null, None).await;
        assert_eq!(status, 200);
        let first = parse(&raw)["items"][0]["id"].as_i64().unwrap();
        let (status, _, raw) = send(
            &app,
            M::GET,
            &format!("{path}?after=9999999"),
            Value::Null,
            None,
        )
        .await;
        assert_eq!(status, 200);
        let page = parse(&raw);
        assert_eq!(page["items"], json!([]));
        assert_eq!(page["cursor"], first);
        checkpoints.push((path, page["cursor"].as_i64().unwrap()));
    }
    assert_eq!(inbound("second").await.0, 202);
    assert_eq!(outbound().await.0, 201);
    for (path, cursor) in checkpoints {
        let (status, _, raw) = send(
            &app,
            M::GET,
            &format!("{path}?after={cursor}"),
            Value::Null,
            None,
        )
        .await;
        assert_eq!(status, 200);
        let page = parse(&raw);
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        assert!(page["cursor"].as_i64().unwrap() > cursor);
        assert_eq!(page["cursor"], page["items"][0]["id"]);
    }
}

#[tokio::test]
async fn retired_label_inputs_cannot_modify_a_post_or_broaden_a_thread_read() {
    let app = api();
    let post = |data| send(&app, Method::POST, "/v1/messages", data, None);
    assert!(
        post(json!({"channel":"triage","text":"Invalid","labels":{"team":"oncall"}}))
            .await
            .0
            .is_client_error()
    );
    let root = parse(&post(json!({"channel":"triage","text":"Root"})).await.2)["id"].clone();
    let reply = parse(&post(json!({"reply_to":root,"text":"Ready"})).await.2)["id"].clone();
    let path = format!("/v1/threads/{reply}?label=team%3Doncall&label=state%3Dready%7Cactive");
    assert!(
        send(&app, Method::GET, &path, Value::Null, None)
            .await
            .0
            .is_client_error()
    );
    let (status, _, body) = send(
        &app,
        Method::GET,
        &format!("/v1/threads/{reply}"),
        Value::Null,
        None,
    )
    .await;
    assert_eq!(status, 200);
    let body = parse(&body);
    assert_eq!(body["message"]["id"], root);
    assert_eq!(body["replies"].as_array().unwrap().len(), 1);
    assert_eq!(body["replies"][0]["id"], reply);
}

#[tokio::test]
async fn explicit_source_targets_create_no_remote_copy_and_cannot_partially_commit() {
    use Method as M;
    let app = api();
    send(
        &app,
        M::POST,
        "/v1/sources",
        json!({"name":"notes","channel":"desk","provider":"local","remote":"archive"}),
        None,
    )
    .await;
    let p = json!({"channel":"desk","text":"publish deliberately","sync_targets":["notes"]});
    let attempt = || {
        send(
            &app,
            M::POST,
            "/v1/messages",
            p.clone(),
            Some("queued-once"),
        )
    };
    assert_eq!(attempt().await.0, 422, "no implicit publishing");
    assert_eq!(
        send(&app, M::GET, "/v1/posts/queued-once", Value::Null, None)
            .await
            .0,
        404
    );
    assert_eq!(
        send(
            &app,
            M::PATCH,
            "/v1/sources/notes",
            json!({"outbound":true}),
            None
        )
        .await
        .0,
        200
    );
    let (s, h, body) = attempt().await;
    assert_eq!(s, 201);
    let id = parse(&body)["id"].clone();
    assert_eq!(h["location"], format!("/v1/messages/{id}"));
    assert_eq!(attempt().await.0, 200);
    let (_, _, outbox) = send(&app, M::GET, "/v1/sources/notes/outbox", Value::Null, None).await;
    assert_eq!(parse(&outbox)["items"].as_array().unwrap().len(), 1);
    let (_, _, body) = send(
        &app,
        M::GET,
        &format!("/v1/messages/{id}/sync"),
        Value::Null,
        None,
    )
    .await;
    let targets = parse(&body);
    assert_eq!(targets["targets"][0]["state"], "pending");
    assert!(targets["targets"][0]["url"].is_null());
    assert_eq!(
        send(
            &app,
            M::POST,
            "/v1/messages",
            json!({"reply_to":id,"text":"wrong","sync_targets":["notes"]}),
            None
        )
        .await
        .0,
        422
    );
}

#[tokio::test]
async fn explicit_provider_joins_use_canonical_offline_lane_ids() {
    use Method as M;
    let app = api();
    for (source, provider, remote) in [
        ("bugs", "github", "team/project"),
        ("chat", "slack", "C123"),
        ("issues", "linear", "DEV"),
    ] {
        assert_eq!(
            send(
                &app,
                M::POST,
                "/v1/sources",
                json!({"name":source,"provider":provider,"remote":remote,"channel":"desk"}),
                None
            )
            .await
            .0,
            201
        );
        assert_eq!(
            send(
                &app,
                M::PATCH,
                &format!("/v1/sources/{source}"),
                json!({"outbound":true}),
                None
            )
            .await
            .0,
            200
        );
    }
    for dest in [
        "bugs:0",
        "bugs:other/repo:21",
        "chat:word",
        "issues:other-team",
    ] {
        assert_eq!(
            send(
                &app,
                M::POST,
                "/v1/messages",
                json!({"channel":"desk","text":"no","sync_targets":[dest]}),
                None
            )
            .await
            .0,
            422,
            "{dest}"
        );
    }
    let (s,_,body)=send(&app,M::POST,"/v1/messages",json!({"channel":"desk","text":"one root","sync_targets":["bugs:#0021","bugs:21","chat:123.456","issues:8A42B68F-0265-4033-89E0-8BA8532CED51"]}),None).await;
    assert_eq!(s, 201);
    let id = parse(&body)["id"].as_i64().unwrap();
    let (_, _, body) = send(
        &app,
        M::GET,
        &format!("/v1/messages/{id}/sync"),
        Value::Null,
        None,
    )
    .await;
    let targets = parse(&body);
    let ids: Vec<_> = targets["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["thread"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["21", "123.456", "8a42b68f-0265-4033-89e0-8ba8532ced51"]
    );
    for source in ["bugs", "chat", "issues"] {
        let (_, _, out) = send(
            &app,
            M::GET,
            &format!("/v1/sources/{source}/outbox"),
            Value::Null,
            None,
        )
        .await;
        assert_eq!(
            parse(&out)["items"],
            json!([]),
            "a join never publishes the root to {source}"
        );
    }
    let join = "/v1/sources/bugs/thread-links?thread=%230022";
    let (status, headers, body) = send(&app, M::PUT, join, json!({"root":id}), None).await;
    assert_eq!(status, 201);
    let location = headers["location"].to_str().unwrap();
    assert_eq!(location, "/v1/sources/bugs/thread-links?thread=22");
    assert_eq!(
        parse(&body),
        parse(&send(&app, M::GET, location, Value::Null, None).await.2)
    );
    assert_eq!(
        send(&app, M::PUT, join, json!({"root":id}), None).await.0,
        200
    );
}

#[tokio::test]
async fn sse_reconnect_resumes_both_seeded_and_cursor_urls_without_replays() {
    use futures_util::StreamExt;
    let app = api();
    let post = |word| {
        send(
            &app,
            Method::POST,
            "/v1/messages",
            json!({"channel":"desk","text":word}),
            None,
        )
    };
    let one = parse(&post("one").await.2)["id"].as_i64().unwrap();
    let two = parse(&post("two").await.2)["id"].as_i64().unwrap();
    send(
        &app,
        Method::POST,
        "/v1/channels",
        json!({"name":"empty"}),
        None,
    )
    .await;
    for suffix in ["?after=0", "?last=2"] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/events{suffix}"))
                    .header("authorization", "Bearer test-local-token")
                    .header("accept", "text/event-stream")
                    .header("last-event-id", one.to_string())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "{suffix}");
        let mut stream = resp.into_body().into_data_stream();
        let next = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let data = std::str::from_utf8(&next).unwrap();
        assert!(data.lines().any(|l| l == format!("id: {two}")), "{data}");
    }
    for (suffix, expect_control) in [
        ("", false),
        ("&all=true", true),
        ("&kind=channel.created", true),
    ] {
        let (s, _, body) = send(
            &app,
            Method::GET,
            &format!("/v1/events?after={two}{suffix}"),
            Value::Null,
            None,
        )
        .await;
        assert_eq!(s, 200);
        let body = parse(&body);
        assert!(body["cursor"].as_i64().unwrap() > two);
        assert_eq!(
            body["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["type"] == "channel.created"),
            expect_control,
            "{body}"
        );
    }
}

#[tokio::test]
async fn generated_openapi_matches_the_request_shapes() {
    use Method as M;
    let app = api();
    let (_, _, spec) = send(&app, M::GET, "/v1/openapi.json", Value::Null, None).await;
    let published = parse(&spec);
    for path in ["/threads", "/threads/{id}", "/messages"] {
        let params = published["paths"][path]["get"]["parameters"]
            .as_array()
            .unwrap();
        assert!(
            params
                .iter()
                .any(|p| p["name"] == "limit" && p["in"] == "query")
        );
    }
    assert!(published["paths"]["/messages"]["post"].is_object());
    fn check_refs(doc: &Value, node: &Value) {
        if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
            assert!(
                doc.pointer(reference.strip_prefix('#').unwrap()).is_some(),
                "{reference}"
            );
        }
        match node {
            Value::Object(fields) => fields.values().for_each(|v| check_refs(doc, v)),
            Value::Array(items) => items.iter().for_each(|v| check_refs(doc, v)),
            _ => {}
        }
    }
    check_refs(&published, &published);
    for (model, accepted, rejected) in [
        (
            "PlanItem",
            json!({"text":"do this","needs":["build",42]}),
            json!({"text":"wrong","thread_id":1}),
        ),
        ("PlanItem", json!({"thread_id":42}), json!({})),
        (
            "NewMessage",
            json!({"channel":"work","text":"ok"}),
            json!({"channel":null,"text":"wrong"}),
        ),
        ("NewTask", json!({"thread_id":4}), json!({"thread_id":0})),
    ] {
        let contract = json!({"$ref":format!("#/components/schemas/{model}"),"components":published["components"]});
        let check = jsonschema::validator_for(&contract).unwrap();
        assert!(check.is_valid(&accepted), "{model}: {accepted}");
        assert!(!check.is_valid(&rejected), "{model}: {rejected}");
    }
    let actor_query = published["paths"]["/actors"]["get"]["parameters"]
        .as_array()
        .unwrap();
    assert!(actor_query.iter().all(|p| p["in"] == "query"));
    assert!(actor_query.iter().any(|p| p["name"] == "id"));
    for (path, methods) in published["paths"].as_object().unwrap() {
        for (_, op) in methods.as_object().unwrap() {
            for p in op["parameters"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|p| p["in"] == "path")
            {
                assert!(
                    path.contains(&format!("{{{}}}", p["name"].as_str().unwrap())),
                    "{path}: {p}"
                );
            }
        }
    }
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
async fn retired_message_filters_fail_closed_and_live_sse_rechecks_revocation() {
    use Method as M;
    use futures_util::StreamExt;
    let app = api();
    for text in ["release", "other"] {
        assert_eq!(
            send(
                &app,
                M::POST,
                "/v1/messages",
                json!({"channel":"ops","text":text}),
                None
            )
            .await
            .0,
            201
        );
    }
    let (status, _, _) = send(
        &app,
        M::GET,
        "/v1/messages?label=ready%3D&label=stage%3Dok%7Cgo",
        Value::Null,
        None,
    )
    .await;
    assert!(status.is_client_error());
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
