use super::*;

#[test]
fn narrower_grant_hides_the_open_root_draft_without_erasing_its_saved_copy() {
    use board_local::{ComposerSettings, Draft, access::Rules};
    let f = Fixture::new();
    let owner = Board::open(&f.db).unwrap();
    owner
        .post("desk", "host", "Still visible", &Default::default())
        .unwrap();
    owner
        .post("private", "host", "Private source", &Default::default())
        .unwrap();
    let grant = owner
        .set_grant(
            "host",
            None,
            "reader",
            &Rules::parse("*=read,post").unwrap(),
        )
        .unwrap();
    let secret = owner
        .issue_credential("host", "reader", None, None)
        .unwrap()
        .token;
    let mut scoped = Board::open(&f.db).unwrap();
    scoped.bind_token(&secret).unwrap();
    let d = Draft {
        channel: "private".into(),
        reply_to: None,
        body: "hidden draft contents".into(),
    };
    scoped
        .save_composer(
            "reader",
            "new",
            None,
            Some((&d, &ComposerSettings::default())),
            Some(&[("private-filename.txt".into(), vec![1, 2])]),
        )
        .unwrap();
    let mut cmd = f.command(
        json!([{"t":0.4,"type":"mark","name":"open"}]),
        DESKTOP,
        "3.0",
    );
    let tree = f.at_mark(cmd.env("BOARD_TOKEN", &secret), "open", || {
        owner
            .set_grant(
                "host",
                Some(grant.id),
                "reader",
                &Rules::parse("desk=read,post").unwrap(),
            )
            .unwrap();
    });
    tree.expect_text("Still visible");
    let rendered = serde_json::to_string(&tree.0).unwrap();
    for secret in [
        "hidden draft contents",
        "private-filename.txt",
        "Private source",
    ] {
        assert!(!rendered.contains(secret));
    }
    owner
        .set_grant(
            "host",
            Some(grant.id),
            "reader",
            &Rules::parse("*=read,post").unwrap(),
        )
        .unwrap();
    let recovered = scoped.load_composer("reader", "new").unwrap().unwrap();
    assert_eq!(recovered.draft.body, d.body);
    assert_eq!(recovered.files[0].0, "private-filename.txt");
}

#[test]
fn unread_dots_and_inline_channel_creation_use_the_same_local_board_after_restart() {
    let f = Fixture::new();
    let db = Board::open(&f.db).unwrap();
    let root = db
        .post("work", "sam", "Morning", &Default::default())
        .unwrap();
    let quiet = db
        .post("notes", "sam", "Notes", &Default::default())
        .unwrap();
    let baseline = f.render(json!([]), "1200x800", "0.4");
    assert!(!baseline.has_key("nav-work-unread"));
    db.reply(root.id, "casey", "News", &Default::default())
        .unwrap();
    db.reply(quiet.id, "casey", "More notes", &Default::default())
        .unwrap();
    let new = f.render(json!([]), "1200x800", "0.4");
    new.expect_key("nav-work-unread");
    new.expect_key("nav-notes-unread");
    let seen = f.render(
        json!([{"t":0.1,"type":"tap","key":"nav-work"}]),
        "1200x800",
        "0.6",
    );
    assert!(!seen.has_key("nav-work-unread"));
    seen.expect_key("nav-notes-unread");
    let added = f.render(
        json!([
            {"t":0.1,"type":"tap","key":"nav-new"},
            {"t":0.3,"paste":"ops/new"},
            {"t":0.5,"type":"key","name":"Enter"}
        ]),
        "1200x800",
        "1.0",
    );
    assert!(!added.has_key("nav-work-unread"));
    added.expect_key("nav-notes-unread");
    added.expect_key("nav-ops:new");
    assert_eq!(db.channel("ops/new").unwrap().threads, 0);
}
