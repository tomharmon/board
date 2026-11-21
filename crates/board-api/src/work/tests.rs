use super::*;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use board_local::Board;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

fn api(board: Board) -> Router {
    crate::routes(App {
        db: Arc::new(Mutex::new(board)),
        actor: "ada/worker".into(),
        token: "test-token".into(),
    })
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}
async fn request(
    router: &Router,
    method: &str,
    path: &str,
    body: Value,
    headers: &[(&str, &str)],
) -> Reply {
    let mut req = Request::builder()
        .method(method)
        .uri(format!("/v1{path}"))
        .header(header::AUTHORIZATION, "Bearer test-token");
    for (key, value) in headers {
        req = req.header(*key, *value)
    }
    let req = if body.is_null() {
        req.body(Body::empty())
    } else {
        req.header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
    };
    let resp = router.clone().oneshot(req.unwrap()).await.unwrap();
    let (parts, body) = resp.into_parts();
    let bytes = to_bytes(body, 2 * 1024 * 1024).await.unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).to_string()))
    };
    Reply {
        status: parts.status,
        headers: parts.headers,
        body,
    }
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn replacement_and_bulk_delivery_routes_preserve_receipts_and_last_active_task_summaries() {
    runtime().block_on(async {
        let router = api(Board::memory().unwrap());
        let config =
            json!({"watchers":[{"name":"ops","filter":{"channel":"ops"},"kind":"message.posted"}]});
        assert_eq!(
            request(&router, "PUT", "/watchers", config.clone(), &[])
                .await
                .status,
            StatusCode::OK
        );
        let made = request(
            &router,
            "POST",
            "/tasks",
            json!({"channel":"ops","text":"first","no_sync":true}),
            &[],
        )
        .await;
        let id = made.body["id"].as_i64().unwrap();
        request(
            &router,
            "POST",
            "/tasks",
            json!({"channel":"ops","text":"second"}),
            &[],
        )
        .await;
        request(
            &router,
            "POST",
            "/messages",
            json!({"reply_to":id,"text":"recent"}),
            &[],
        )
        .await;
        let latest = request(
            &router,
            "GET",
            "/tasks?latest=true&limit=1",
            Value::Null,
            &[],
        )
        .await;
        assert_eq!(latest.body["tasks"][0]["id"], id);
        assert_eq!(latest.body["tasks"][0]["last"]["text"], "recent");
        assert_eq!(latest.body["tasks"][0]["replies"], 1);
        assert!(latest.body["next_after"].is_null());
        assert_eq!(
            request(
                &router,
                "GET",
                "/tasks?latest=true&after=0",
                Value::Null,
                &[]
            )
            .await
            .status,
            StatusCode::BAD_REQUEST
        );
        let pulled = request(&router, "POST", "/watcher-matches", json!({"limit":5}), &[]).await;
        let matches = pulled.body["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(
            request(&router, "PUT", "/watchers", config, &[])
                .await
                .status,
            StatusCode::OK
        );
        assert_eq!(
            request(&router, "POST", "/watcher-matches", json!({}), &[])
                .await
                .body["matches"],
            json!([])
        );
        let head = &matches[0];
        let receipt = format!("\"{}\"", head["receipt"].as_str().unwrap());
        assert_eq!(
            request(
                &router,
                "PUT",
                &format!("/watchers/ops/acknowledgements/{}", head["event"]["cursor"]),
                json!({}),
                &[("If-Match", &receipt)]
            )
            .await
            .status,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            request(&router, "POST", "/watcher-matches", json!({}), &[])
                .await
                .body["matches"][0]["event"]["message"]["text"],
            "recent"
        );
    })
}

#[test]
fn work_collections_validate_page_and_return_structured_failures() {
    runtime().block_on(work_pages());
}

#[test]
fn task_collection_finds_reverse_dependencies_and_filters_before_pagination() {
    runtime().block_on(async {
        let db = Board::memory().unwrap();
        let make = |channel: &str, tag: &str, needs: &[i64]| {
            work::create_task(
                &db,
                "ada",
                TaskTarget::New {
                    channel,
                    text: "review",
                    options: &PostOptions {
                        labels: Labels::from([("role".into(), tag.into())]),
                        ..Default::default()
                    },
                },
                needs,
                None,
            )
            .unwrap()
            .id
        };
        let pre = make("ops", "lead", &[]);
        make("ops/ci", "skip", &[pre]);
        let first = make("ops", "review", &[pre]);
        let second = make("ops/ci", "verify", &[pre]);
        make("ops-old", "review", &[pre]);
        make("ops/ci", "review", &[]);
        let router = api(db);
        let mine = request(
            &router,
            "POST",
            "/tasks",
            json!({"channel":"worker","text":"mine"}),
            &[],
        )
        .await;
        assert_eq!(mine.status, StatusCode::CREATED);
        let exact_actor = request(&router, "GET", "/tasks?by=me&limit=1", Value::Null, &[]).await;
        assert_eq!(exact_actor.body["tasks"][0]["id"], mine.body["id"]);
        assert!(exact_actor.body["next_after"].is_null());
        let query = format!(
            "/tasks?channel=ops%2F&needs={pre}&state=waiting&label=role%3Dreview%7Cverify&limit=1"
        );
        let page = request(&router, "GET", &query, Value::Null, &[]).await;
        assert_eq!(page.status, StatusCode::OK);
        assert_eq!(page.body["tasks"][0]["id"], first);
        assert_eq!(page.body["next_after"], first);
        let next = request(
            &router,
            "GET",
            &format!("{query}&after={first}"),
            Value::Null,
            &[],
        )
        .await;
        assert_eq!(next.body["cursor"], page.body["cursor"]);
        assert_eq!(next.body["tasks"][0]["id"], second);
        assert!(next.body["next_after"].is_null());
        let exact = request(
            &router,
            "GET",
            &format!("/tasks?channel=ops&needs={pre}&label=role"),
            Value::Null,
            &[],
        )
        .await;
        assert_eq!(exact.body["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(exact.body["tasks"][0]["id"], first);
    });
}

async fn work_pages() {
    let router = api(Board::memory().unwrap());
    for name in ["two", "one"] {
        let w = request(&router, "POST", "/watchers", json!({"name":name}), &[]).await;
        assert_eq!(w.status, StatusCode::CREATED);
    }
    let head = request(&router, "GET", "/watchers?limit=1", Value::Null, &[]).await;
    assert_eq!(head.body["next_after"], "one");
    let duplicate = request(
        &router,
        "POST",
        "/watchers",
        json!({"name":"two","kind":"message.posted"}),
        &[],
    )
    .await;
    assert_eq!(duplicate.status, StatusCode::CONFLICT);
    assert_eq!(
        duplicate.headers[header::CONTENT_TYPE],
        "application/problem+json"
    );
    assert_eq!(duplicate.body["code"], "watcher_conflict");
    let rest = request(&router, "GET", "/watchers?after=one", Value::Null, &[]).await;
    assert_eq!(rest.body["watchers"][0]["name"], "two");
    assert_eq!(rest.body["cursor"], head.body["cursor"]); // A failed create left no control event.
    assert!(rest.body["next_after"].is_null());
    for path in [
        "/tasks?after=-1",
        "/watchers?limit=501",
        "/schedules?limit=0",
    ] {
        let rejected = request(&router, "GET", path, Value::Null, &[]).await;
        assert_eq!(rejected.status, StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(
            rejected.headers[header::CONTENT_TYPE],
            "application/problem+json"
        );
    }
    let gone = request(&router, "GET", "/schedules/521", Value::Null, &[]).await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    for due in [epoch + 500, epoch] {
        assert_eq!(
            request(
                &router,
                "POST",
                "/schedules",
                json!({"channel":"a","text":"hi","due":due}),
                &[]
            )
            .await
            .status,
            StatusCode::CREATED
        );
    }
    let page = request(&router, "GET", "/schedules?limit=1", Value::Null, &[]).await;
    let id = page.body["next_after"].as_i64().unwrap();
    assert_eq!(page.body["schedules"][0]["due"], epoch + 500); // Ordered by stable ID, not mutable due time.
    let page = request(
        &router,
        "GET",
        &format!("/schedules?after={id}"),
        Value::Null,
        &[],
    )
    .await;
    assert_eq!(page.body["schedules"][0]["due"], epoch);
}

#[test]
fn task_state_and_assignment_resources_check_their_inputs() {
    runtime().block_on(task_edits());
}

async fn task_edits() {
    let router = api(Board::memory().unwrap());
    let new = request(
        &router,
        "POST",
        "/tasks",
        json!({"channel":"work","text":"routing"}),
        &[],
    )
    .await;
    let id = new.body["id"].as_i64().unwrap();
    let url = format!("/tasks/{id}");
    let needs = format!("{url}/dependencies");
    let cycle = request(&router, "PUT", &needs, json!({"needs":[id]}), &[]).await;
    assert_eq!(cycle.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(cycle.body["code"], "invalid_needs");
    let assign = format!("{url}/assignment");
    assert_eq!(
        request(&router, "PUT", &assign, json!({}), &[])
            .await
            .status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        request(
            &router,
            "PUT",
            &assign,
            json!({"assignee":"ada/other"}),
            &[]
        )
        .await
        .body["assignee"],
        "ada/other"
    );
    assert_eq!(
        request(&router, "POST", &format!("{url}/claim"), json!({}), &[])
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let pooled = request(&router, "PUT", &assign, json!({"assignee":null}), &[]).await;
    assert_eq!(pooled.status, StatusCode::OK);
    assert!(pooled.body["assignee"].is_null());
    let before = request(&router, "GET", "/tasks", Value::Null, &[])
        .await
        .body["cursor"]
        .clone();
    assert_eq!(
        request(&router, "PUT", &assign, json!({"assignee":null}), &[])
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        request(&router, "PUT", &needs, json!({"needs":[]}), &[])
            .await
            .status,
        StatusCode::OK
    );
    let after = request(&router, "GET", "/tasks", Value::Null, &[]).await;
    assert_eq!(after.body["cursor"], before);
    assert_eq!(
        request(
            &router,
            "PATCH",
            &url,
            json!({"state":"blocked","reason":"review"}),
            &[]
        )
        .await
        .body["state"],
        "blocked"
    );
    assert_eq!(
        request(&router, "PATCH", &url, json!({"state":"ready"}), &[])
            .await
            .body["state"],
        "ready"
    );
    assert_eq!(
        request(
            &router,
            "PATCH",
            &url,
            json!({"state":"canceled","reason":"not needed"}),
            &[]
        )
        .await
        .body["state"],
        "canceled"
    );
    let no_resume = request(&router, "PATCH", &url, json!({"state":"ready"}), &[]).await;
    assert_eq!(no_resume.status, StatusCode::CONFLICT, "{}", no_resume.body);
    assert_eq!(
        request(
            &router,
            "POST",
            "/task-claims",
            json!({"channel":"work"}),
            &[]
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
}

#[test]
fn task_resources_page_replay_and_fence_claims() {
    runtime().block_on(task_round_trip());
}

#[test]
fn task_attachments_are_message_files_and_retries_keep_the_original_bytes() {
    runtime().block_on(task_files());
}

async fn task_files() {
    let router = api(Board::memory().unwrap());
    // The encoded request exceeds the framework's default 2 MiB request body limit.
    let bytes = "z".repeat(1_600_000);
    let body = json!({"channel":"evidence","attachments":[{"name":"trace.log","data_base64":STANDARD.encode(&bytes)}]});
    let headers = &[("idempotency-key", "trace")];
    let first = request(&router, "POST", "/tasks", body.clone(), headers).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    let id = first.body["id"].as_i64().unwrap();
    let replay = request(&router, "POST", "/tasks", body, headers).await;
    assert_eq!(replay.status, StatusCode::OK);
    assert_eq!(replay.body["id"], id);
    let changed = request(
        &router,
        "POST",
        "/tasks",
        json!({"channel":"evidence","attachments":[{"name":"trace.log","data_base64":"YQ=="}]}),
        headers,
    )
    .await;
    assert_eq!(changed.status, StatusCode::OK);
    assert_eq!(changed.body["id"], id);
    let root = request(&router, "GET", &format!("/messages/{id}"), Value::Null, &[]).await;
    let attachment = &root.body["attachments"][0];
    assert_eq!(attachment["name"], "trace.log");
    assert_eq!(attachment["size"], 1_600_000);
    let url = format!("/attachments/{}", attachment["id"]);
    let download = request(&router, "GET", &url, Value::Null, &[]).await;
    assert_eq!(download.status, StatusCode::OK);
    assert_eq!(download.headers[header::CONTENT_TYPE], "text/plain");
    assert_eq!(download.body.as_str().unwrap(), bytes);
}

async fn task_round_trip() {
    let router = api(Board::memory().unwrap());
    let new = json!({"channel":"work/build","text":"assemble"});
    let made = request(
        &router,
        "POST",
        "/tasks",
        new.clone(),
        &[("idempotency-key", "build")],
    )
    .await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.body);
    let id = made.body["id"].as_i64().unwrap();
    assert_eq!(made.headers[header::LOCATION], format!("/v1/tasks/{id}"));
    let replay = request(
        &router,
        "POST",
        "/tasks",
        new,
        &[("idempotency-key", "build")],
    )
    .await;
    assert_eq!(replay.status, StatusCode::OK);
    assert_eq!(
        replay.headers[header::LOCATION],
        made.headers[header::LOCATION]
    );
    assert_eq!(replay.body["id"], id);
    let bad = request(
        &router,
        "POST",
        "/tasks",
        json!({"channel":"elsewhere","text":"changed"}),
        &[("idempotency-key", "build")],
    )
    .await;
    assert_eq!(bad.status, StatusCode::CONFLICT);
    let dep = request(
        &router,
        "POST",
        "/tasks",
        json!({"channel":"work/build","text":"package","needs":[id]}),
        &[],
    )
    .await;
    assert_eq!(dep.body["state"], "waiting");
    let dep_id = dep.body["id"].as_i64().unwrap();

    let first = request(
        &router,
        "GET",
        "/tasks?channel=work%2F&limit=1",
        Value::Null,
        &[],
    )
    .await;
    assert_eq!(first.body["next_after"], id);
    assert!(first.body["cursor"].as_i64().unwrap() >= dep_id);
    let second = request(
        &router,
        "GET",
        &format!("/tasks?after={id}"),
        Value::Null,
        &[],
    )
    .await;
    assert_eq!(second.body["tasks"][0]["id"], dep_id);

    let path = format!("/tasks/{id}/claim");
    let claim = request(&router, "POST", &path, json!({"lease_seconds":30}), &[]).await;
    assert_eq!(claim.status, StatusCode::CREATED, "{}", claim.body);
    let tag = claim.headers[header::ETAG].to_str().unwrap();
    let renewal = request(
        &router,
        "PATCH",
        &path,
        json!({"epoch":claim.body["epoch"],"token":claim.body["claim_token"],"lease_seconds":90}),
        &[],
    )
    .await;
    assert_eq!(renewal.status, StatusCode::OK);
    assert_eq!(renewal.body["claim_token"], claim.body["claim_token"]);
    assert_eq!(renewal.headers[header::CACHE_CONTROL], "no-store");
    let occupied = request(&router, "POST", &path, json!({}), &[]).await;
    assert_eq!(occupied.status, StatusCode::CONFLICT);
    let lease = request(&router, "GET", &path, Value::Null, &[]).await;
    assert!(lease.body.get("claim_token").is_none());
    assert_eq!(
        request(&router, "DELETE", &path, Value::Null, &[])
            .await
            .status,
        StatusCode::PRECONDITION_REQUIRED
    );
    let lost = request(
        &router,
        "DELETE",
        &path,
        Value::Null,
        &[("if-match", "\"9999:stale\"")],
    )
    .await;
    assert_eq!(lost.status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(
        request(&router, "DELETE", &path, Value::Null, &[("if-match", tag)])
            .await
            .status,
        StatusCode::NO_CONTENT
    );

    let again = request(
        &router,
        "POST",
        "/task-claims",
        json!({"channel":"work/"}),
        &[],
    )
    .await;
    assert_eq!(again.body["id"], id, "{}", again.body);
    let update = format!("/tasks/{id}");
    let old = request(
        &router,
        "PATCH",
        &update,
        json!({"state":"completed","epoch":claim.body["epoch"],"token":claim.body["claim_token"]}),
        &[],
    )
    .await;
    assert_eq!(old.status, StatusCode::CONFLICT);
    let done=request(&router,"PATCH",&update,json!({"state":"completed","result":"ok","epoch":again.body["epoch"],"token":again.body["claim_token"]}),&[]).await;
    assert_eq!(done.body["result"], "ok", "{}", done.body);
    assert_eq!(
        request(
            &router,
            "GET",
            &format!("/tasks/{dep_id}"),
            Value::Null,
            &[]
        )
        .await
        .body["state"],
        "ready"
    );
}

#[test]
fn schedule_and_watcher_resources_survive_server_and_worker_restart() {
    runtime().block_on(automation_round_trip());
}

#[test]
fn task_batches_have_navigable_locations_and_atomic_keyed_retries() {
    runtime().block_on(batch_resources());
}

#[test]
fn waiting_queue_claim_releases_connection_and_filters_tasks_by_root_author_and_assignment() {
    runtime().block_on(async {
        let router = api(Board::memory().unwrap());
        let query = router.clone();
        let waiting = tokio::spawn(async move {
            request(
                &query,
                "POST",
                "/task-claims",
                json!({"channel":"ops","wait_seconds":3}),
                &[],
            )
            .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let t = request(
            &router,
            "POST",
            "/tasks",
            json!({"channel":"ops","text":"release","assignee":"ada/worker"}),
            &[],
        )
        .await;
        let claimed = waiting.await.unwrap();
        assert_eq!(claimed.status, StatusCode::CREATED);
        assert_eq!(claimed.body["id"], t.body["id"]);
        let list = request(
            &router,
            "GET",
            "/tasks?by=ada&for=ada%2Fworker",
            Value::Null,
            &[],
        )
        .await;
        assert_eq!(list.body["tasks"][0]["id"], t.body["id"]);
        let other = request(&router, "GET", "/tasks?for=ada%2Fother", Value::Null, &[]).await;
        assert_eq!(other.body["tasks"], json!([]));
    })
}

#[test]
fn http_client_can_recover_minted_claims_and_repeat_without_another_lease() {
    runtime().block_on(async{
        let router=api(Board::memory().unwrap());
        let headers=[("idempotency-key","create-plus-claim")];
        let input=json!({"channel":"ops","text":"first","claim":true,"token":"caller-minted-token","lease_seconds":30});
        let one=request(&router,"POST","/tasks",input.clone(),&headers).await;
        assert_eq!(one.status,StatusCode::CREATED);assert_eq!(one.body["state"],"claimed");
        let again=request(&router,"POST","/tasks",input,&headers).await;
        assert_eq!(again.status,StatusCode::OK);assert_eq!(again.body,one.body);
        let recovered=request(&router,"POST","/task-claims/recover",json!({"token":"caller-minted-token"}),&[]).await;
        assert_eq!(recovered.body,one.body);
        let second=request(&router,"POST","/tasks",json!({"channel":"ops","text":"second"}),&[]).await;
        let hold=format!("/tasks/{}/claim",second.body["id"]);
        let token=json!({"token":"another-minted-token"});
        let win=request(&router,"POST",&hold,token.clone(),&[]).await;
        assert_eq!(win.status,StatusCode::CREATED);
        let retry=request(&router,"POST",&hold,token,&[]).await;
        assert_eq!(retry.status,StatusCode::OK);assert_eq!(retry.body,win.body);
        let tag=win.headers[header::ETAG].to_str().unwrap();
        assert_eq!(request(&router,"DELETE",&hold,Value::Null,&[("if-match",tag)]).await.status,StatusCode::NO_CONTENT);
        let stale=request(&router,"POST","/task-claims/recover",json!({"token":"another-minted-token"}),&[]).await;
        assert_eq!(stale.body["code"],"claim_lost");
    })
}

async fn batch_resources() {
    let router = api(Board::memory().unwrap());
    let key = [("idempotency-key", "release/first build")];
    let body = json!({"channel":"work","tasks":[{"text":"ship","key":"ship","needs":[{"key":"test"}]},{"text":"test","key":"test"}]});
    let first = request(&router, "POST", "/task-batches", body.clone(), &key).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    let location = first.headers[header::LOCATION]
        .to_str()
        .unwrap()
        .strip_prefix("/v1")
        .unwrap();
    let found = request(&router, "GET", location, Value::Null, &[]).await;
    assert_eq!(found.body, first.body);
    assert_eq!(first.body["tasks"][0]["state"], "waiting");
    assert_eq!(
        request(&router, "POST", "/task-batches", body, &key)
            .await
            .status,
        StatusCode::OK
    );
    let altered = request(
        &router,
        "POST",
        "/task-batches",
        json!({"channel":"work","tasks":[{"text":"changed"}]}),
        &key,
    )
    .await;
    assert_eq!(altered.status, StatusCode::CONFLICT);
    let cycle = json!({"channel":"work","tasks":[{"text":"one","key":"a","needs":["b"]},{"text":"two","key":"b","needs":["a"]}]});
    assert_eq!(
        request(
            &router,
            "POST",
            "/task-batches",
            cycle,
            &[("idempotency-key", "cycle")]
        )
        .await
        .status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        request(&router, "GET", "/task-batches/cycle", Value::Null, &[])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&router, "GET", "/tasks", Value::Null, &[])
            .await
            .body["tasks"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let dot = request(
        &router,
        "POST",
        "/task-batches",
        json!({"channel":"work","tasks":[{"text":"dot is a valid client key"}]}),
        &[("idempotency-key", "..")],
    )
    .await;
    let location = dot.headers[header::LOCATION]
        .to_str()
        .unwrap()
        .strip_prefix("/v1")
        .unwrap();
    assert!(location.contains("?key="));
    assert_eq!(
        request(&router, "GET", location, Value::Null, &[])
            .await
            .body,
        dot.body
    );
}

#[test]
fn error_and_skip_ack_outcomes_leave_other_lanes_available() {
    runtime().block_on(watch_outcomes());
}
async fn watch_outcomes() {
    let router = api(Board::memory().unwrap());
    request(
        &router,
        "POST",
        "/watchers",
        json!({"name":"jobs","kind":"message.posted"}),
        &[],
    )
    .await;
    let a = request(
        &router,
        "POST",
        "/messages",
        json!({"channel":"ops","text":"a"}),
        &[],
    )
    .await;
    let root = a.body["id"].as_i64().unwrap();
    let child = request(
        &router,
        "POST",
        "/messages",
        json!({"reply_to":root,"text":"later"}),
        &[],
    )
    .await
    .body["id"]
        .as_i64()
        .unwrap();
    let b = request(
        &router,
        "POST",
        "/messages",
        json!({"channel":"ops","text":"b"}),
        &[],
    )
    .await
    .body["id"]
        .as_i64()
        .unwrap();
    let mut ids = Vec::new();
    for _ in 0..2 {
        let d = request(&router, "POST", "/watchers/jobs/deliveries", json!({}), &[]).await;
        ids.push((
            d.body["event"]["cursor"].as_i64().unwrap(),
            d.headers[header::ETAG].to_str().unwrap().to_owned(),
        ));
    }
    assert_eq!((ids[0].0, ids[1].0), (root, b));
    let url = format!("/watchers/jobs/acknowledgements/{root}");
    let proof = [("if-match", ids[0].1.as_str())];
    assert_eq!(
        request(
            &router,
            "PUT",
            &url,
            json!({"skip":true,"error":"both"}),
            &proof
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(&router, "PUT", &url, json!({"skip":true}), &proof)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    let url = format!("/watchers/jobs/acknowledgements/{b}");
    assert_eq!(
        request(
            &router,
            "PUT",
            &url,
            json!({"error":"busy"}),
            &[("if-match", &ids[1].1)]
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(&router, "GET", "/watchers/jobs/next", Value::Null, &[])
            .await
            .body["cursor"],
        child
    );
    assert_eq!(
        request(&router, "GET", "/watchers/jobs/failures", Value::Null, &[])
            .await
            .body["failures"],
        json!([])
    );
}

async fn automation_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board.db");
    let router = api(Board::open(&path).unwrap());
    let made=request(&router,"POST","/watchers",json!({"name":"pings","filter":{"channel":"ops/","labels":{"ping":"yes"}},"kind":"message.posted"}),&[]).await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.body);
    assert_eq!(made.headers[header::LOCATION], "/v1/watchers/pings");
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let make_schedule = json!({"channel":"ops/ci","due":epoch-30,"every":60,"times":2,"text":"check","labels":{"ping":"yes"}});
    let s = request(&router, "POST", "/schedules", make_schedule, &[]).await;
    assert_eq!(s.status, StatusCode::CREATED, "{}", s.body);
    let sid = s.body["id"].as_i64().unwrap();
    assert_eq!(
        request(&router, "GET", "/watchers/pings/next", Value::Null, &[])
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    drop(router);

    let worker = Board::open(&path).unwrap();
    assert_eq!(work::dispatch_due(&worker).unwrap(), 1);
    assert_eq!(work::dispatch_due(&worker).unwrap(), 0);
    drop(worker);
    let router = api(Board::open(&path).unwrap());
    let first = request(&router, "GET", "/watchers/pings/next", Value::Null, &[]).await;
    assert_eq!(first.body["type"], "message.posted", "{}", first.body);
    let cursor = first.body["cursor"].as_i64().unwrap();
    assert_eq!(first.body["payload"]["schedule"]["id"], sid);
    drop(router);
    let router = api(Board::open(&path).unwrap());
    assert_eq!(
        request(&router, "GET", "/watchers/pings/next", Value::Null, &[])
            .await
            .body,
        first.body
    );
    let path_ack = format!("/watchers/pings/acknowledgements/{cursor}");
    let lease = request(
        &router,
        "POST",
        "/watchers/pings/deliveries",
        json!({"lease_seconds":30}),
        &[],
    )
    .await;
    let tag = lease.headers[header::ETAG].to_str().unwrap();
    assert_eq!(lease.body["event"], first.body);
    assert_eq!(
        request(
            &router,
            "PUT",
            &path_ack,
            Value::Null,
            &[("if-match", "\"stale\"")]
        )
        .await
        .status,
        StatusCode::PRECONDITION_FAILED
    );
    assert_eq!(
        request(&router, "GET", "/watchers/pings/next", Value::Null, &[])
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(&router, "PUT", &path_ack, Value::Null, &[])
            .await
            .status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(&router, "PUT", &path_ack, Value::Null, &[("if-match", tag)])
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(&router, "PUT", &path_ack, Value::Null, &[("if-match", tag)])
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(&router, "GET", "/watchers/pings/next", Value::Null, &[])
            .await
            .status,
        StatusCode::NO_CONTENT
    );

    assert_eq!(
        request(
            &router,
            "DELETE",
            &format!("/schedules/{sid}"),
            Value::Null,
            &[]
        )
        .await
        .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(
            &router,
            "GET",
            &format!("/schedules/{sid}"),
            Value::Null,
            &[]
        )
        .await
        .body["state"],
        "canceled"
    );
    assert_eq!(
        request(&router, "GET", "/schedules", Value::Null, &[])
            .await
            .body["schedules"],
        json!([])
    );
    assert_eq!(
        request(&router, "DELETE", "/watchers/pings", Value::Null, &[])
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(&router, "GET", "/watchers/pings", Value::Null, &[])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
}
