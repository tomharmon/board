use board_local::{Board, Filter, PostOptions, errors, policy::Rule, sources::*};
use serde_json::json;

fn rule(name: &str, n: i64, script: &str) -> Rule {
    Rule {
        name: name.into(),
        priority: n,
        script: script.into(),
    }
}
#[test]
fn installed_pipeline_is_ordered_idempotent_private_and_transactional_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("board");
    let b = Board::open(&db).unwrap();
    b.set_write_rule("p",&rule("first",0,r#"if write.operation == "post" { #{text:write.text+"!",emit:[#{channel:"audit",text:write.text}]} }"#)).unwrap();
    b.set_write_rule("p",&rule("second",1,r#"if write.text == "bad!" { #{drop:"rejected"} } else if write.operation == "post" { #{text:write.text+"?",labels:#{checked:"yes"}} }"#)).unwrap();
    let opts = PostOptions {
        key: Some("stable".into()),
        no_sync: true,
        ..Default::default()
    };
    let a = b.post("dev", "p", "hello", &opts).unwrap();
    assert_eq!(a.text, "hello!?");
    assert_eq!(a.labels["checked"], "yes");
    drop(b);
    let b = Board::open(&db).unwrap();
    let replay = b.post("dev", "p", "hello", &opts).unwrap();
    assert_eq!(a.id, replay.id);
    let head = b.cursor().unwrap();
    assert_eq!(
        errors::classify(&b.post("dev", "p", "bad", &Default::default()).unwrap_err()).1,
        "dropped"
    );
    assert!(b.edit(a.id, "p", "oops", Some(0)).is_err());
    assert_eq!(head, b.cursor().unwrap());
    let effects = b
        .messages(
            &Filter {
                channel: Some("audit".into()),
                ..Default::default()
            },
            10,
        )
        .unwrap();
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].actor, "board:middleware");
    assert_eq!(effects[0].text, "hello");
    assert!(effects[0].no_sync);
    b.set_write_rule("p", &rule("loop", 2, "loop {}")).unwrap();
    let head = b.cursor().unwrap();
    assert_eq!(
        errors::classify(
            &b.post("dev", "p", "bounded", &Default::default())
                .unwrap_err()
        )
        .1,
        "rule_error"
    );
    assert_eq!(b.cursor().unwrap(), head);
}
#[test]
fn dropped_import_has_a_durable_terminal_receipt_and_does_not_block_next_root() {
    let b = Board::memory().unwrap();
    b.create_source("p", "remote", Provider::Local, "tickets", "dev")
        .unwrap();
    b.set_write_rule("p",&rule("reject",0,r#"if write.source != () && write.source.key == "remote" && write.text == "noise" { #{drop:"filtered"} }"#)).unwrap();
    for (key, body) in [("one", "noise"), ("two", "keep")] {
        let e:Envelope=serde_json::from_value(json!({"key":key,"import":{"thread":key,"external":"root","author":"p","change":{"op":"post","text":body}}})).unwrap();
        b.enqueue_imports("p", "remote", &[e], None).unwrap();
    }
    assert_eq!(b.drain_source("p", "remote", 20).unwrap(), (1, 0));
    let queue = b.inbox("p", "remote", Direction::In, 0, 10).unwrap();
    assert_eq!(queue[0].state, "skipped");
    assert_eq!(queue[1].state, "done");
    assert!(b.find_source("p", "remote", "one", None).unwrap().is_none());
}
