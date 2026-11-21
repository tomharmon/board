use anyhow::Result;
use board_local::{
    Board,
    automation::{ClaimTarget, TaskTarget, claim, create_task, get_task},
    errors::classify,
    sources::{Envelope, Provider},
};
use serde_json::{Value, json};

fn event(key: &str, who: &str, change: Value) -> Result<Envelope> {
    Ok(serde_json::from_value(json!({
        "key":key,"import":{"thread":"T","external":"item","author":who,"change":change}
    }))?)
}

#[test]
fn an_authoritative_source_edit_names_the_editor_and_holds_another_actors_claim() -> Result<()> {
    let b = Board::memory()?;
    b.create_source("owner", "incoming", Provider::Local, "room", "work")?;
    let root = b.import_source(
        "owner",
        "incoming",
        &event("original", "maker", json!({"op":"post","text":"work"}))?,
    )?;
    b.create_source("owner", "mirror", Provider::Local, "other-room", "work")?;
    b.attach_source("owner", "mirror", "T", "item", root.id, None)?;
    for (key, change) in [
        ("edit-copy", json!({"op":"edit","text":"mirror-only edit"})),
        ("delete-copy", json!({"op":"delete"})),
    ] {
        b.import_source("owner", "mirror", &event(key, "moderator", change)?)?;
    }
    assert_eq!(b.get(root.id)?.revision, root.revision);
    create_task(&b, &root.actor, TaskTarget::Existing(root.id), &[], None)?;
    let t = claim(
        &b,
        &root.actor,
        ClaimTarget::Id(root.id),
        300,
        &Default::default(),
    )?;
    assert_eq!(t.state, "claimed");
    let editor = "source:incoming/moderator";
    let before = b.cursor()?;
    assert_eq!(
        classify(&b.edit(root.id, editor, "not allowed", None).unwrap_err()).1,
        "not_owned"
    );
    assert_eq!(
        classify(&b.delete(root.id, editor, None).unwrap_err()).1,
        "not_owned"
    );
    assert_eq!(b.cursor()?, before);
    let edit = event(
        "moderation",
        "moderator",
        json!({"op":"edit","text":"changed externally"}),
    )?;
    let changed = b.import_source("owner", "incoming", &edit)?;
    assert_eq!(changed.actor, root.actor);
    let events = b.events(before, 50)?;
    assert_eq!(
        events
            .iter()
            .find(|e| e.kind == "message.edited")
            .unwrap()
            .actor,
        editor
    );
    let task = get_task(&b, root.id)?;
    assert_eq!(task.state, "blocked");
    assert!(task.holder.is_none());
    assert_eq!(task.hold_kind.as_deref(), Some("edited"));
    let head = b.cursor()?;
    b.import_source("owner", "incoming", &edit)?;
    assert_eq!(b.cursor()?, head);

    let removed = b.import_source(
        "owner",
        "incoming",
        &event("removed", "moderator", json!({"op":"delete"}))?,
    )?;
    assert!(removed.deleted);
    assert_eq!(removed.actor, root.actor);
    assert_eq!(
        b.events(head, 50)?
            .iter()
            .find(|e| e.kind == "message.deleted")
            .unwrap()
            .actor,
        editor
    );
    Ok(())
}
