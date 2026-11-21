use super::*;
mod coverage;
use board_local::PostOptions;
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    thread,
    time::Instant,
};

struct Mock {
    base: String,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    handle: Option<thread::JoinHandle<()>>,
}
impl Mock {
    fn new(expected: Vec<(&'static str, u16, Value)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        let handle = thread::spawn(move || {
            for (want, status, response) in expected {
                let end = Instant::now() + Duration::from_secs(8);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((s, _)) => break s,
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < end =>
                        {
                            thread::sleep(Duration::from_millis(5))
                        }
                        Err(e) => panic!("missing fixture request {want}: {e}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut len = 0;
                let mut auth = false;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    let (k, v) = line.split_once(':').unwrap();
                    if k.eq_ignore_ascii_case("content-length") {
                        len = v.trim().parse().unwrap();
                    }
                    if k.eq_ignore_ascii_case("authorization") {
                        auth = matches!(v.trim(), "Bearer test" | "test");
                    }
                }
                assert_eq!(
                    auth,
                    !want.starts_with("POST /upload-raw "),
                    "temporary upload URLs must not receive a bearer"
                );
                assert!(first.starts_with(want), "expected {want}, got {first}");
                let mut body = vec![0; len];
                reader.read_exact(&mut body).unwrap();
                let decoded = if len == 0 {
                    Value::Null
                } else {
                    serde_json::from_slice(&body).unwrap()
                };
                seen.lock().unwrap().push((first, decoded));
                let buf = serde_json::to_vec(&response).unwrap();
                write!(stream,"HTTP/1.1 {status} response\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",buf.len()).unwrap();
                stream.write_all(&buf).unwrap();
            }
        });
        Self {
            base,
            requests,
            handle: Some(handle),
        }
    }
    fn finish(mut self) -> Vec<(String, Value)> {
        self.handle.take().unwrap().join().unwrap();
        self.requests.lock().unwrap().clone()
    }
}
fn setup(kind: Provider, remote: &str, m: &Mock) -> (Board, Connector) {
    let db = Board::memory().unwrap();
    db.create_source("ada", "test", kind.clone(), remote, "team/review")
        .unwrap();
    (
        db,
        Connector::new(kind, m.base.clone(), Some("test".into())).unwrap(),
    )
}
#[test]
fn github_ci_updates_only_the_matching_head_and_redelivery_is_harmless() {
    let db = Board::memory().unwrap();
    db.create_source("p", "repo", Provider::Github, "a/b", "dev")
        .unwrap();
    let repo = json!({"full_name":"a/b","id":5});
    let opened = json!({"repository":repo,"action":"opened","pull_request":{"number":7,"title":"PR","state":"open","head":{"sha":"abc"}}});
    ingest(&db, "p", "repo", "pull_request", "1", opened).unwrap();
    db.drain_source("p", "repo", 9).unwrap();
    let workflow = |sha: &str, state: &str| json!({"repository":repo,"action":"completed","workflow_run":{"id":11,"head_sha":sha,"name":"Build","path":".github/workflows/build.yml","pull_requests":[{"number":7}],"conclusion":state}});
    for (key, head, state) in [
        ("2", "old", "failure"),
        ("3", "abc", "failure"),
        ("3", "abc", "failure"),
        ("4", "abc", "success"),
    ] {
        ingest(&db, "p", "repo", "workflow_run", key, workflow(head, state)).unwrap();
        db.drain_source("p", "repo", 9).unwrap();
        let root = db.find_source("p", "repo", "7", None).unwrap().unwrap();
        assert_eq!(root.labels.contains_key("ci.5-7.build"), key == "3");
        assert_eq!(
            db.conversation(root.id, &Default::default())
                .unwrap()
                .replies
                .len(),
            usize::from(key != "2")
        );
    }
}
#[test]
fn private_files_require_an_explicit_verified_message_and_never_forward_upload_credentials() {
    let f = |channel: &str, url: &str| json!({"ok":true,"file":{"id":"F1","name":"note.txt","size":7,"mimetype":"text/plain","url_private_download":url,"shares":{"private":{channel:[{"ts":"10.1"}]}}}});
    let mock = Mock::new(vec![
        (
            "GET /conversations.replies?",
            200,
            json!({"ok":true,"messages":[{"ts":"10.1","user":"U1","text":"Here is a file"}]}),
        ),
        (
            "GET /files.info?",
            200,
            f("C2", "https://files.slack.com/test-bytes"),
        ),
        (
            "GET /files.info?",
            200,
            f("C1", "https://example.invalid/private"),
        ),
        (
            "GET /files.info?",
            200,
            f("C1", "https://files.slack.com/test-bytes"),
        ),
        ("GET /test-bytes ", 200, json!("proof")),
        (
            "POST /files.getUploadURLExternal?",
            200,
            json!({"ok":true,"file_id":"F2","upload_url":"https://files.slack.com/upload-raw"}),
        ),
        ("POST /upload-raw ", 200, json!({})),
        (
            "POST /files.completeUploadExternal ",
            200,
            json!({"ok":true,"files":[{"id":"F2"}]}),
        ),
    ]);
    let (db, c) = setup(Provider::Slack, "C1", &mock);
    c.pull(&db, "ada", "test", Some("10.1")).unwrap();
    let root = db
        .find_source("ada", "test", "10.1", None)
        .unwrap()
        .unwrap();
    for _ in 0..2 {
        assert!(
            c.fetch_file(&db, "ada", "test", "10.1", "10.1", "F1")
                .is_err()
        );
    }
    let file = c
        .fetch_file(&db, "ada", "test", "10.1", "10.1", "F1")
        .unwrap();
    assert_eq!(
        c.fetch_file(&db, "ada", "test", "10.1", "10.1", "F1")
            .unwrap()
            .id,
        file.id
    );
    assert_eq!(db.get(root.id).unwrap().attachments.len(), 1);
    assert_eq!(db.download_as("ada", file.id).unwrap().1, br#""proof""#);
    db.set_outbound("ada", "test", true).unwrap();
    assert!(c.send_next(&db, "ada", "test").unwrap().is_none());
    db.stage_source_file("ada", "test", "10.1", root.id, file.id)
        .unwrap();
    c.send_next(&db, "ada", "test").unwrap();
    assert_eq!(db.source_files("ada", "test", root.id).unwrap().len(), 2);
    let requests = mock.finish();
    assert_eq!(requests[6].1, "proof");
    assert_eq!(requests[7].1["thread_ts"], "10.1");
}
#[test]
fn slack_preview_public_join_and_aggregate_reactions_are_explicit() {
    let page = json!({"ok":true,"messages":[{"ts":"10.1","user":"U1","text":"root","reactions":[{"name":"eyes","users":["U2"]}]}]});
    let mock = Mock::new(vec![
        (
            "GET /conversations.info?",
            200,
            json!({"ok":true,"channel":{"id":"C1","is_channel":true,"is_private":false,"is_shared":false,"is_member":true}}),
        ),
        ("GET /conversations.replies?", 200, page.clone()),
        (
            "GET /conversations.info?",
            200,
            json!({"ok":true,"channel":{"id":"C1","is_channel":true,"is_private":true,"is_shared":false}}),
        ),
        (
            "GET /conversations.info?",
            200,
            json!({"ok":true,"channel":{"id":"C1","is_channel":true,"is_private":false,"is_shared":false,"is_member":false}}),
        ),
        ("POST /conversations.join ", 200, json!({"ok":true})),
        ("GET /conversations.replies?", 200, page),
        ("POST /reactions.add ", 200, json!({"ok":true})),
        ("POST /reactions.remove ", 200, json!({"ok":true})),
    ]);
    let (db, c) = setup(Provider::Slack, "C1", &mock);
    let before = db.cursor().unwrap();
    assert_eq!(
        c.preview(&db, "ada", "test", Some("10.1")).unwrap()["records"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(c.join_public_slack(&db, "ada", "test").is_err());
    c.join_public_slack(&db, "ada", "test").unwrap();
    assert_eq!(before, db.cursor().unwrap());
    c.pull(&db, "ada", "test", Some("10.1")).unwrap();
    let root = db
        .find_source("ada", "test", "10.1", None)
        .unwrap()
        .unwrap();
    assert_eq!(root.reactions["eyes"], ["source:test/U2"]);
    db.set_outbound("ada", "test", true).unwrap();
    db.react(root.id, "ada", "eyes", false).unwrap();
    db.react(root.id, "bob", "eyes", false).unwrap();
    c.send_next(&db, "ada", "test").unwrap().unwrap();
    assert!(c.send_next(&db, "ada", "test").unwrap().is_none());
    db.react(root.id, "ada", "eyes", true).unwrap();
    assert!(c.send_next(&db, "ada", "test").unwrap().is_none());
    db.react(root.id, "bob", "eyes", true).unwrap();
    c.send_next(&db, "ada", "test").unwrap().unwrap();
    assert_eq!(
        mock.finish().last().unwrap().1,
        json!({"channel":"C1","timestamp":"10.1","name":"eyes"})
    );
}
#[test]
fn publishing_a_new_issue_then_replaying_backfill_and_redacting_the_root() {
    let mock = Mock::new(vec![
        ("POST /repos/test/board/issues ", 201, json!({"number":32})),
        (
            "POST /repos/test/board/issues/32/comments ",
            201,
            json!({"id":41}),
        ),
        ("PATCH /repos/test/board/issues/32 ", 200, json!({})),
        ("PATCH /repos/test/board/issues/32 ", 200, json!({})),
    ]);
    let (db, api) = setup(Provider::Github, "test/board", &mock);
    let root = db
        .post("team/review", "ada", "Title\nBody", &Default::default())
        .unwrap();
    let reply = db
        .reply(root.id, "ada", "Existing reply", &Default::default())
        .unwrap();
    assert!(db.start_source_thread("ada", "test", root.id).is_err());
    db.set_outbound("ada", "test", true).unwrap();
    let first = db.start_source_thread("ada", "test", root.id).unwrap();
    assert_eq!(
        db.start_source_thread("ada", "test", root.id).unwrap().id,
        first.id
    );
    api.send_next(&db, "ada", "test").unwrap().unwrap();
    api.send_next(&db, "ada", "test").unwrap().unwrap();
    assert_eq!(
        db.find_source("ada", "test", "32", Some("comment:41"))
            .unwrap()
            .unwrap()
            .id,
        reply.id
    );
    assert_eq!(db.backfill_source("ada", "test", "32").unwrap(), 0);
    db.edit(root.id, "ada", "New title\nNew body", None)
        .unwrap();
    api.send_next(&db, "ada", "test").unwrap().unwrap();
    db.delete(root.id, "ada", None).unwrap();
    api.send_next(&db, "ada", "test").unwrap().unwrap();
    let req = mock.finish();
    assert_eq!(req[0].1, json!({"title":"Title","body":"Body"}));
    assert_eq!(req[2].1["body"], "New body");
    assert_eq!(req[3].1["title"], "[deleted on Board]");
}
#[test]
fn offline_native_files_handle_deletion_reactions_deduplication_and_wrong_sources() {
    let db = Board::memory().unwrap();
    db.create_source("ada", "chat", Provider::Slack, "C1", "dev")
        .unwrap();
    let event = |id: &str, v: Value| {
        let body = json!({"token":"not for storage","event":v});
        ingest(&db, "ada", "chat", "slack", id, body).unwrap();
        db.drain_source("ada", "chat", 20).unwrap()
    };
    let msg = json!({"type":"message","channel":"C1","user":"U1","ts":"10.1","text":"original"});
    assert_eq!(event("a", msg.clone()), (1, 0));
    assert_eq!(event("a", msg.clone()), (0, 0));
    event(
        "b",
        json!({"type":"message","channel":"C1","user":"U2","ts":"11.1","thread_ts":"10.1","text":"answer"}),
    );
    event(
        "c",
        json!({"type":"reaction_added","user":"U1","event_ts":"12.1","item":{"type":"message","channel":"C1","ts":"11.1"},"reaction":"eyes"}),
    );
    let child = db
        .find_source("ada", "chat", "10.1", Some("11.1"))
        .unwrap()
        .unwrap();
    assert_eq!(child.reactions.len(), 1);
    event(
        "d",
        json!({"type":"message","subtype":"message_deleted","deleted_ts":"11.1","channel":"C1","event_ts":"13.1"}),
    );
    assert!(db.get(child.id).unwrap().deleted);
    assert!(
        db.inbox("ada", "chat", Direction::In, 0, 20)
            .unwrap()
            .iter()
            .all(|j| j.raw.get("token").is_none())
    );
    assert!(
        ingest(
            &db,
            "ada",
            "chat",
            "slack",
            "e",
            json!({"event":{"type":"message","channel":"OTHER"}})
        )
        .is_err()
    );
    db.create_source("ada", "git", Provider::Github, "one/r", "dev")
        .unwrap();
    let mut gh = json!({"action":"opened","repository":{"full_name":"other/r"},"issue":{"number":2,"title":"Hello","state":"open"}});
    assert!(ingest(&db, "ada", "git", "issues", "f", gh.clone()).is_err());
    gh["repository"]["full_name"] = json!("one/r");
    ingest(&db, "ada", "git", "issues", "f", gh).unwrap();
    assert_eq!(db.drain_source("ada", "git", 20).unwrap(), (1, 0));
    db.create_source("ada", "issues", Provider::Linear, "TEAM", "dev")
        .unwrap();
    let mut lin = json!({"action":"create","type":"Issue","data":{"id":"uuid","identifier":"WRONG-1","title":"Hello"}});
    assert!(ingest(&db, "ada", "issues", "linear", "g", lin.clone()).is_err());
    lin["data"]["identifier"] = json!("TEAM-1");
    ingest(&db, "ada", "issues", "linear", "g", lin).unwrap();
    assert_eq!(db.drain_source("ada", "issues", 20).unwrap(), (1, 0));
}
#[test]
fn github_pr_reviews_inline_parents_and_opt_in_reply_use_real_http_protocol() {
    let mock = Mock::new(vec![
        (
            "GET /repos/acme/r/issues/7 ",
            200,
            json!({"number":7,"state":"open","title":"Patch","body":"Test","user":{"id":14},"html_url":"https://github.com/acme/r/pull/7","pull_request":{} }),
        ),
        (
            "GET /repos/acme/r/pulls/7 ",
            200,
            json!({"merged":false,"draft":false}),
        ),
        ("GET /repos/acme/r/issues/7/comments?", 200, json!([])),
        (
            "GET /repos/acme/r/pulls/7/reviews?",
            200,
            json!([{"id":8,"state":"APPROVED","user":{"id":22},"body":"looks good","submitted_at":"2025-01-01T00:00:00Z"}]),
        ),
        (
            "GET /repos/acme/r/pulls/7/comments?",
            200,
            json!([
          {"id":92,"user":{"id":21},"path":"src/lib.rs","line":10,"body":"next","in_reply_to_id":91,"html_url":"https://github.com/acme/r/pull/7#r92"},
          {"id":91,"user":{"id":20},"path":"src/lib.rs","line":10,"body":"first","html_url":"https://github.com/acme/r/pull/7#r91"}]),
        ),
        (
            "POST /repos/acme/r/pulls/7/comments/91/replies ",
            201,
            json!({"id":99,"html_url":"https://github.com/acme/r/pull/7#r99"}),
        ),
    ]);
    let (db, api) = setup(Provider::Github, "acme/r", &mock);
    let r = api.pull(&db, "ada", "test", Some("7")).unwrap();
    assert_eq!((r.records, r.failed), (4, 0));
    let root = db.find_source("ada", "test", "7", None).unwrap().unwrap();
    assert_eq!(root.labels["review"], "approved");
    let first = db
        .find_source("ada", "test", "7", Some("inline:91"))
        .unwrap()
        .unwrap();
    assert_eq!(
        db.find_source("ada", "test", "7", Some("inline:92"))
            .unwrap()
            .unwrap()
            .reply_to,
        Some(first.id)
    );
    assert!(
        db.inbox("ada", "test", Direction::Out, 0, 10)
            .unwrap()
            .is_empty()
    );
    db.set_outbound("ada", "test", true).unwrap();
    let m = db
        .reply(first.id, "ada", "follow up", &PostOptions::default())
        .unwrap();
    api.send_next(&db, "ada", "test").unwrap();
    assert_eq!(
        db.source_copies("ada", "test", m.id).unwrap()[0].external,
        "inline:99"
    );
    assert_eq!(mock.finish().last().unwrap().1["body"], "follow up");
}
#[test]
fn slack_resume_page_and_uncertain_write_preserve_deliveries_without_blind_retry() {
    let mock = Mock::new(vec![
        (
            "GET /conversations.replies?",
            200,
            json!({"ok":true,"messages":[{"ts":"100.000001","user":"U1","text":"root"}],"has_more":true,"response_metadata":{"next_cursor":"continue-here"}}),
        ),
        (
            "GET /conversations.replies?",
            200,
            json!({"ok":true,"messages":[{"ts":"101.000001","user":"U2","text":"reply"}],"has_more":false}),
        ),
        ("POST /chat.postMessage ", 503, json!({"ok":false})),
    ]);
    let (db, api) = setup(Provider::Slack, "C123", &mock);
    api.pull(&db, "ada", "test", Some("100.000001")).unwrap();
    let root = db
        .find_source("ada", "test", "100.000001", None)
        .unwrap()
        .unwrap();
    db.set_outbound("ada", "test", true).unwrap();
    db.reply(
        root.id,
        "ada",
        "intentionally shared",
        &PostOptions::default(),
    )
    .unwrap();
    assert!(api.send_next(&db, "ada", "test").is_err());
    assert_eq!(
        db.inbox("ada", "test", Direction::Out, 0, 10).unwrap()[0].state,
        "uncertain"
    );
    assert!(api.send_next(&db, "ada", "test").unwrap().is_none());
    let req = mock.finish();
    assert!(req[1].0.contains("cursor=continue-here"));
    assert_eq!(req[2].1["thread_ts"], "100.000001");
}
#[test]
fn linear_uses_canonical_issue_id_and_paginates_before_linking_nested_comments() {
    let issue = |comments: Value| json!({"data":{"issue":{"id":"id-7","identifier":"ENG-7","title":"Bug","url":"https://linear.app/demo/issue/ENG-7","creator":{"id":"u"},"team":{"key":"ENG"},"state":{"name":"Backlog"},"comments":comments}}});
    let mock = Mock::new(vec![
        (
            "POST /graphql ",
            200,
            issue(
                json!({"nodes":[{"id":"c2","parent":{"id":"c1"},"body":"reply","user":{"id":"a"}}],"pageInfo":{"hasNextPage":true,"endCursor":"p"}}),
            ),
        ),
        (
            "POST /graphql ",
            200,
            issue(
                json!({"nodes":[{"id":"c1","body":"discussion","user":{"id":"b"}}],"pageInfo":{"hasNextPage":false}}),
            ),
        ),
        (
            "POST /graphql ",
            200,
            json!({"data":{"issue":{"id":"id-7","team":{"key":"ENG"}},"comment":{"id":"c1","issue":{"id":"id-7"}}}}),
        ),
        (
            "POST /graphql ",
            200,
            json!({"data":{"commentCreate":{"success":true,"comment":{"id":"c3","url":"https://linear.app/demo/issue/ENG-7#c3"}}}}),
        ),
    ]);
    let (db, api) = setup(Provider::Linear, "ENG", &mock);
    api.pull(&db, "ada", "test", Some("ENG-7")).unwrap();
    let c1 = db
        .find_source("ada", "test", "id-7", Some("comment:c1"))
        .unwrap()
        .unwrap();
    assert_eq!(
        db.find_source("ada", "test", "id-7", Some("comment:c2"))
            .unwrap()
            .unwrap()
            .reply_to,
        Some(c1.id)
    );
    db.set_outbound("ada", "test", true).unwrap();
    db.reply(c1.id, "ada", "ack", &Default::default()).unwrap();
    api.send_next(&db, "ada", "test").unwrap();
    let req = mock.finish();
    assert_eq!(req[1].1["variables"]["after"], "p");
    assert_eq!(
        req[3].1["variables"]["input"],
        json!({"issueId":"id-7","parentId":"c1","body":"ack"})
    );
}

#[test]
fn linear_explicit_joins_verify_the_current_team_before_any_mutation() {
    let id = "30facd12-fd14-4bde-aaa1-ba7b616853d2";
    let issue = |team| json!({"data":{"issue":{"id":id,"identifier":"ENG-7","team":{"key":team}}}});
    let create = |b: &Board| {
        b.set_outbound("ada", "test", true).unwrap();
        let root = b
            .post_with_targets(
                "team/review",
                "ada",
                "Never send this local root",
                &Default::default(),
                &[format!("test:{id}")],
            )
            .unwrap()
            .0;
        assert!(
            b.inbox("ada", "test", Direction::Out, 0, 10)
                .unwrap()
                .is_empty()
        );
        b.reply(root.id, "ada", "Limited to this team", &Default::default())
            .unwrap()
    };
    let denied = Mock::new(vec![("POST /graphql ", 200, issue("OTHER"))]);
    let (db, api) = setup(Provider::Linear, "ENG", &denied);
    let root = create(&db);
    assert!(
        api.send_next(&db, "ada", "test")
            .unwrap_err()
            .to_string()
            .contains("nothing sent")
    );
    let job = &db.inbox("ada", "test", Direction::Out, 0, 10).unwrap()[0];
    assert_eq!(job.state, "pending");
    assert!(job.error.as_deref().unwrap().contains("team"));
    assert!(db.source_copies("ada", "test", root.id).unwrap().is_empty());
    let seen = denied.finish();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].1["query"].as_str().unwrap().starts_with("query "));
    assert_eq!(seen[0].1["variables"], json!({"id":id}));
    assert!(!seen[0].1.to_string().contains("Limited to this team"));

    let allowed = Mock::new(vec![
        ("POST /graphql ", 200, issue("ENG")),
        (
            "POST /graphql ",
            200,
            json!({"data":{"commentCreate":{"success":true,"comment":{"id":"c3"}}}}),
        ),
        ("POST /graphql ", 200, issue("MOVED")),
    ]);
    let (db, api) = setup(Provider::Linear, "ENG", &allowed);
    let first = create(&db);
    assert_eq!(
        api.send_next(&db, "ada", "test").unwrap().unwrap().state,
        "done"
    );
    assert_eq!(
        db.source_copies("ada", "test", first.id).unwrap()[0].external,
        "comment:c3"
    );
    db.reply(
        first.id,
        "ada",
        "Do not follow a moved issue",
        &Default::default(),
    )
    .unwrap();
    assert!(api.send_next(&db, "ada", "test").is_err());
    let seen = allowed.finish();
    assert_eq!(seen.len(), 3);
    assert!(seen[0].1["query"].as_str().unwrap().starts_with("query "));
    assert!(
        seen[1].1["query"]
            .as_str()
            .unwrap()
            .starts_with("mutation ")
    );
    assert_eq!(
        seen[1].1["variables"]["input"],
        json!({"issueId":id,"body":"Limited to this team"})
    );
    assert!(seen[2].1["query"].as_str().unwrap().starts_with("query "));
    assert!(!seen[2].1.to_string().contains("Do not follow"));
}

#[test]
fn linear_rejects_a_mapped_parent_comment_belonging_to_another_issue() {
    let fixture = Mock::new(vec![(
        "POST /graphql ",
        200,
        json!({"data":{"issue":{"id":"issue-1","team":{"key":"ENG"}},"comment":{"id":"other-comment","issue":{"id":"issue-2"}}}}),
    )]);
    let (db, api) = setup(Provider::Linear, "ENG", &fixture);
    let root = db
        .post("team/review", "ada", "Root", &Default::default())
        .unwrap();
    db.attach_source("ada", "test", "issue-1", "issue", root.id, None)
        .unwrap();
    let mapped = db
        .reply(root.id, "ada", "Reference", &Default::default())
        .unwrap();
    db.attach_source(
        "ada",
        "test",
        "issue-1",
        "comment:other-comment",
        mapped.id,
        None,
    )
    .unwrap();
    db.set_outbound("ada", "test", true).unwrap();
    db.reply(mapped.id, "ada", "Private context", &Default::default())
        .unwrap();
    assert!(api.send_next(&db, "ada", "test").is_err());
    let records = fixture.finish();
    assert_eq!(
        records[0].1["variables"],
        json!({"id":"issue-1","comment":"other-comment"})
    );
    assert!(
        records[0].1["query"]
            .as_str()
            .unwrap()
            .starts_with("query ")
    );
    assert!(!records[0].1.to_string().contains("Private context"));
    assert!(
        db.inbox("ada", "test", Direction::Out, 0, 10).unwrap()[0]
            .error
            .as_deref()
            .unwrap()
            .contains("outside the verified issue")
    );
}
