use anyhow::Result;
use board_local::{
    Board, Filter,
    errors::classify,
    policy::Rule,
    sources::{Envelope, Provider},
};
use serde_json::{Value, json};

const VALID: &str = "```json#metric@1\n{\"value\":42}\n```";
const UNKNOWN: &str = "```json#unregistered@1\nnot JSON\n```";
fn shape(b: &Board) -> Result<()> {
    b.register_schema_as(
        "metric",
        1,
        "",
        &json!({"type":"object","required":["value"],"properties":{"value":{"type":"integer"}}}),
        "admin",
    )?;
    Ok(())
}
fn envelope(key: &str, external: &str, parent: Option<&str>, change: Value) -> Result<Envelope> {
    Ok(serde_json::from_value(
        json!({"key":key,"import":{"author":"U1","external":external,"thread":"T","parent":parent,"change":change}}),
    )?)
}

#[test]
fn platform_prose_is_not_a_local_schema_assertion_even_on_edits_and_replays() -> Result<()> {
    let b = Board::memory()?;
    shape(&b)?;
    b.create_source("admin", "incoming", Provider::Local, "inbox", "updates")?;
    let first = envelope("one", "root", None, json!({"op":"post","text":VALID}))?;
    let root = b.import_source("admin", "incoming", &first)?;
    assert!(
        root.blocks.is_empty(),
        "externally supplied JSON was promoted to a trusted block"
    );
    let reply = b.import_source(
        "admin",
        "incoming",
        &envelope(
            "two",
            "reply",
            Some("root"),
            json!({"op":"post","text":UNKNOWN}),
        )?,
    )?;
    assert_eq!(reply.text, UNKNOWN);
    assert!(reply.blocks.is_empty());
    let edited = b.import_source(
        "admin",
        "incoming",
        &envelope("three", "root", None, json!({"op":"edit","text":UNKNOWN}))?,
    )?;
    assert_eq!(edited.text, UNKNOWN);
    assert!(edited.blocks.is_empty());
    let head = b.cursor()?;
    assert_eq!(b.import_source("admin", "incoming", &first)?.id, root.id);
    assert_eq!(b.cursor()?, head);
    // A deliberate local edit is a new native text revision, even if its original came from a platform.
    let local = b.edit(root.id, &root.actor, VALID, Some(edited.revision))?;
    assert_eq!(local.blocks.len(), 1);
    assert!(b.get(root.id)?.blocks.len() == 1);
    let changed = b.event_feed(0, 50, &Filter::default(), None, false)?.1;
    assert!(
        changed
            .iter()
            .filter_map(|e| e.message.as_ref())
            .all(|m| m.revision == local.revision || m.blocks.is_empty())
    );
    Ok(())
}

#[test]
fn native_blocks_are_checked_before_middleware_and_again_after_a_rewrite() -> Result<()> {
    let b = Board::memory()?;
    shape(&b)?;
    let root = b.post("dev", "person", VALID, &Default::default())?;
    b.set_write_rule("admin",&Rule{name:"replace".into(),priority:0,
        script:r#"if write.text.contains("unregistered") { #{text:"plain",emit:[#{channel:"audit",text:"side effect"}]} } else if write.text == "make invalid" { #{text:"```json#unknown@1\n{}\n```"} }"#.into()})?;
    let at = b.cursor()?;
    for text in [UNKNOWN, "make invalid"] {
        for err in [
            b.post("dev", "person", text, &Default::default())
                .unwrap_err(),
            b.reply(root.id, "person", text, &Default::default())
                .unwrap_err(),
            b.edit(root.id, "person", text, Some(root.revision))
                .unwrap_err(),
        ] {
            assert_eq!(classify(&err).1, "schema_not_found");
        }
    }
    assert_eq!(b.cursor()?, at);
    assert_eq!(b.get(root.id)?.text, VALID);
    assert_eq!(b.channels()?.len(), 1);
    Ok(())
}

#[test]
fn v13_upgrade_removes_trust_from_imported_blocks_without_touching_text_or_the_event_log()
-> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("provenance");
    let b = Board::open(&path)?;
    shape(&b)?;
    b.create_source("admin", "incoming", Provider::Local, "inbox", "updates")?;
    let native = b.post("updates", "local", VALID, &Default::default())?;
    let imported = b.import_source(
        "admin",
        "incoming",
        &envelope("root", "root", None, json!({"op":"post","text":VALID}))?,
    )?;
    let head = b.cursor()?;
    drop(b);
    let fixture = rusqlite::Connection::open(&path)?;
    let blocks = serde_json::to_string(&native.blocks)?;
    fixture.execute_batch("DROP TABLE legacy_platform_revisions; PRAGMA user_version=13;")?;
    fixture.execute(
        "UPDATE messages SET blocks=? WHERE id=?",
        rusqlite::params![blocks, imported.id],
    )?;
    fixture.execute(
        "UPDATE events SET message=json_set(message,'$.blocks',json(?)) WHERE message_id=?",
        rusqlite::params![blocks, imported.id],
    )?;
    let snapshot = |c: &rusqlite::Connection| {
        c.query_row(
            "SELECT message FROM events WHERE cursor=?",
            [imported.id],
            |r| r.get::<_, String>(0),
        )
    };
    let before = snapshot(&fixture)?;
    drop(fixture);
    let b = Board::open(&path)?;
    assert_eq!(b.cursor()?, head);
    assert_eq!(b.get(native.id)?.blocks, native.blocks);
    let current = b.get(imported.id)?;
    assert_eq!(current.text, VALID);
    assert!(current.blocks.is_empty());
    assert!(
        b.event_feed(0, 100, &Default::default(), None, true)?
            .1
            .iter()
            .filter_map(|e| e.message.as_ref())
            .filter(|m| m.id == imported.id)
            .all(|m| m.blocks.is_empty())
    );
    assert_eq!(snapshot(&rusqlite::Connection::open(&path)?)?, before);
    let newer = b.edit(imported.id, &imported.actor, VALID, None)?;
    assert_eq!(newer.blocks, native.blocks);
    assert_eq!(b.get(imported.id)?.blocks, native.blocks);
    Ok(())
}
