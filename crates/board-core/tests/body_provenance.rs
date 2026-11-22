use anyhow::Result;
use board_local::{
    Board, Filter,
    errors::classify,
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
fn native_blocks_reject_unknown_schema_without_partial_writes() -> Result<()> {
    let b = Board::memory()?;
    shape(&b)?;
    let root = b.post("dev", "person", VALID, &Default::default())?;
    let at = b.cursor()?;
    for err in [
        b.post("dev", "person", UNKNOWN, &Default::default())
            .unwrap_err(),
        b.reply(root.id, "person", UNKNOWN, &Default::default())
            .unwrap_err(),
        b.edit(root.id, "person", UNKNOWN, Some(root.revision))
            .unwrap_err(),
    ] {
        assert_eq!(classify(&err).1, "schema_not_found");
    }
    assert_eq!(b.cursor()?, at);
    assert_eq!(b.get(root.id)?.text, VALID);
    assert_eq!(b.channels()?.len(), 1);
    Ok(())
}
