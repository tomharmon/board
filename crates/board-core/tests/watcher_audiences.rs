use anyhow::Result;
use board_local::{
    Board, Filter,
    access::Rules,
    automation::{add_watcher_as, next_event},
};

#[test]
fn identical_actor_selectors_do_not_share_a_directory_between_credentials_of_the_same_person()
-> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("watchers.db");
    let b = Board::open(&path)?;
    b.set_actor_profile("person-7", "reviewer", "Mira")?;
    b.set_grant("owner", None, "reader", &Rules::parse("*=read")?)?;
    let open = |cap| -> Result<Board> {
        let token = b.issue_credential("owner", "reader", cap, None)?;
        let mut db = Board::open(&path)?;
        db.bind_token(&token.token)?;
        Ok(db)
    };
    let full = open(None)?;
    let cap = Rules::parse("work=read")?;
    let narrow = open(Some(&cap))?;
    let filter = Filter {
        by: Some("ann".into()),
        mentions: Some("reviewer".into()),
        channel: Some("work".into()),
        ..Default::default()
    };
    for (board, name) in [(&full, "full"), (&narrow, "restricted")] {
        add_watcher_as(
            board,
            "reader",
            name,
            filter.clone(),
            Some("message.posted"),
        )?;
    }
    add_watcher_as(
        &narrow,
        "reader",
        "explicit",
        Filter {
            mentions: Some("@[person-7]".into()),
            ..filter.clone()
        },
        Some("message.posted"),
    )?;
    add_watcher_as(
        &full,
        "reader",
        "compound",
        Filter {
            text: Some("changed".into()),
            ..filter
        },
        Some("message.edited"),
    )?;
    let root = b.post("work", "ann", "hello @[person-7]", &Default::default())?;
    assert_eq!(next_event(&full, "full")?.unwrap().cursor, root.id);
    assert!(next_event(&narrow, "restricted")?.is_none());
    assert_eq!(next_event(&narrow, "explicit")?.unwrap().cursor, root.id);
    b.edit(root.id, "ann", "changed without a mention", None)?;
    // A mention in the old version and matching text in the new version aren't one match.
    assert!(next_event(&full, "compound")?.is_none());
    Ok(())
}
