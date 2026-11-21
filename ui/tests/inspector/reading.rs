use super::*;
use board_local::{access::Rules, sources::Provider};

fn scoped(f: &Fixture, token: &str, steps: Value, end: &str) -> Tree {
    let output = f
        .command(steps, DESKTOP, end)
        .env("BOARD_TOKEN", token)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    f.tree()
}

#[test]
fn sidebar_private_channel_restricts_delegates_without_pretending_to_hide_from_the_os_owner() {
    let f = Fixture::new();
    let db = Board::open(&f.db).unwrap();
    db.set_grant(
        "owner",
        None,
        "visitor",
        &Rules::parse("*=read,post").unwrap(),
    )
    .unwrap();
    let token = db
        .issue_credential("owner", "visitor", None, None)
        .unwrap()
        .token;
    let shown=f.render(json!([{"t":0.1,"type":"tap","key":"nav-new"},{"t":0.35,"type":"tap","key":"new-channel-private"}]), DESKTOP,"0.7");
    shown.expect_key("new-channel-private");
    shown.expect_text("Anyone with direct access to the database can still read it.");
    let t=f.render(json!([{"t":0.1,"type":"tap","key":"nav-new"},{"t":0.35,"paste":"release/embargo"},{"t":0.55,"type":"tap","key":"new-channel-private"},{"t":0.8,"type":"tap","key":"submit-channel"}]),DESKTOP,"1.15");
    t.expect_key("nav-release:embargo");
    let confidential = db
        .post(
            "release/embargo",
            "reader",
            "Held release note",
            &Default::default(),
        )
        .unwrap();
    db.post("public", "reader", "Public tracker", &Default::default())
        .unwrap();
    let mut other = Board::open(&f.db).unwrap();
    other.bind_token(&token).unwrap();
    assert!(other.get(confidential.id).is_err());
    let other = scoped(
        &f,
        &token,
        json!([{"t":0.1,"type":"tap","key":"nav-new"}]),
        "0.6",
    );
    other.expect_key("new-channel-name");
    other.expect_key("nav-public");
    assert!(!other.has_key("nav-release:embargo"));
    assert!(!other.has_key("new-channel-private"));
    assert!(
        !other
            .0
            .iter()
            .any(|el| el.to_string().contains("Held release note"))
    );
}

#[test]
fn losing_manage_closes_a_selected_private_channel_form_without_downgrading_to_public() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let grant = b
        .set_grant("owner", None, "reader", &Rules::parse("*=all").unwrap())
        .unwrap();
    let token = b
        .issue_credential("owner", "reader", None, None)
        .unwrap()
        .token;
    let mut cmd=f.command(json!([{"t":0.1,"type":"tap","key":"nav-new"},{"t":0.3,"paste":"classified"},{"t":0.5,"type":"tap","key":"new-channel-private"},{"t":0.75,"type":"mark","name":"private-form"}]),DESKTOP,"2.9");
    cmd.env("BOARD_TOKEN", token);
    let t = f.at_mark(&mut cmd, "private-form", || {
        b.set_grant(
            "owner",
            Some(grant.id),
            "reader",
            &Rules::parse("*=read,post").unwrap(),
        )
        .unwrap();
    });
    t.expect_text("Private channel creation is no longer permitted");
    assert!(!t.has_key("submit-channel"));
    assert!(!b.channels().unwrap().iter().any(|c| c.name == "classified"));
}

#[test]
fn filtered_home_shows_exact_visible_replies_but_channel_search_shows_the_thread() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let root = b
        .post("work", "writer", "Release checklist", &Default::default())
        .unwrap();
    let reply = b
        .reply(root.id, "writer", "needle in a reply", &Default::default())
        .unwrap();
    for i in 1..=42 {
        b.reply(
            root.id,
            "writer",
            &format!("Later reply {i}"),
            &Default::default(),
        )
        .unwrap();
    }
    b.post(
        "sealed",
        "writer",
        "needle in a private channel",
        &Default::default(),
    )
    .unwrap();
    b.set_grant("owner", None, "reader", &Rules::parse("work=read").unwrap())
        .unwrap();
    let token = b
        .issue_credential("owner", "reader", None, None)
        .unwrap()
        .token;
    let find = json!([{"t":0.1,"type":"key","cmd":true,"name":"f"},{"t":0.4,"paste":"needle"}]);
    let home = scoped(&f, &token, find.clone(), "0.8");
    home.expect_key(&format!("activity-reply-{}", reply.id));
    home.expect_text(&reply.text);
    assert!(!home.has_key(&format!("thread-{}", root.id)));
    assert!(
        !home
            .0
            .iter()
            .any(|e| e.to_string().contains("private channel"))
    );
    let mut open = find.as_array().unwrap().clone();
    open.push(json!({"t":0.75,"type":"key","name":"Enter"}));
    let exact = scoped(&f, &token, Value::Array(open), "1.2");
    exact.expect_key("thread-pane");
    exact.expect_key(&format!("inspect-{}", reply.id));
    exact.expect_key("newer-replies");
    assert!(!exact.0.iter().any(|el| el["text"] == "Later reply 42"));
    let channel = scoped(
        &f,
        &token,
        json!([{"t":0.1,"type":"tap","key":"nav-work"},{"t":0.35,"type":"key","cmd":true,"name":"f"},{"t":0.6,"paste":"needle"}]),
        "1.0",
    );
    channel.expect_key(&format!("thread-{}", root.id));
    assert!(!channel.has_key(&format!("activity-reply-{}", reply.id)));
}

#[test]
fn thread_displays_reader_safe_sync_destinations_and_removes_them_on_live_revocation() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    let root = b
        .post("work", "writer", "Published locally", &Default::default())
        .unwrap();
    b.create_source("owner", "mirror", Provider::Local, "release-room", "work")
        .unwrap();
    b.attach_source(
        "owner",
        "mirror",
        "discussion",
        "posted",
        root.id,
        Some("https://example.invalid/a?secret=MUST_NOT_LEAK"),
    )
    .unwrap();
    let grant = b
        .set_grant("owner", None, "reader", &Rules::parse("work=read").unwrap())
        .unwrap();
    let token = b
        .issue_credential("owner", "reader", None, None)
        .unwrap()
        .token;
    let nav = json!({"t":0.15,"type":"tap","key":format!("thread-{}", root.id)});
    let t = scoped(&f, &token, json!([nav]), "0.85");
    t.expect_key("thread-destinations");
    t.expect_text("release-room");
    t.expect_text("confirmed");
    assert!(
        !t.0.iter()
            .any(|el| el.to_string().contains("MUST_NOT_LEAK"))
    );
    let mut cmd = f.command(
        json!([nav,{"t":0.5,"type":"mark","name":"reader-open"}]),
        DESKTOP,
        "2.8",
    );
    cmd.env("BOARD_TOKEN", token);
    let t = f.at_mark(&mut cmd, "reader-open", || {
        b.set_grant(
            "owner",
            Some(grant.id),
            "reader",
            &Rules::parse("other=read").unwrap(),
        )
        .unwrap();
    });
    assert!(!t.has_key("thread-destinations"));
    assert!(!t.0.iter().any(|el| el.to_string().contains("release-room")
        || el.to_string().contains("Published locally")));
}

#[test]
fn a_scrolled_back_feed_announces_external_messages_without_moving_the_reader() {
    let f = Fixture::new();
    let b = Board::open(&f.db).unwrap();
    for i in 0..38 {
        b.post(
            "work",
            "reader",
            &format!("Notebook {i}"),
            &Default::default(),
        )
        .unwrap();
    }
    let steps = json!([{"t":0.2,"type":"wheel","x":500,"y":240,"dy":18000},{"t":0.8,"type":"mark","name":"reading"}]);
    let before = f
        .render(json!([steps[0]]), DESKTOP, "0.8")
        .bounds("thread-1");
    for jump in [false, true] {
        let mut steps = steps.as_array().unwrap().clone();
        if jump {
            steps.push(json!({"t":2.75,"type":"tap","key":"feed-new-messages"}));
        }
        let mut cmd = f.command(Value::Array(steps), DESKTOP, "3.5");
        let mut id = 0;
        let tree = f.at_mark(&mut cmd, "reading", || {
            id = b
                .post(
                    "work",
                    "other",
                    "Arrived while reading",
                    &Default::default(),
                )
                .unwrap()
                .id;
        });
        let viewport = tree.bounds("feed-scroll");
        let last = tree.bounds(&format!("thread-{id}"));
        if jump {
            assert!(!tree.has_key("feed-new-messages"));
            assert!(
                last.y >= viewport.y && last.bottom() <= viewport.bottom() + 2.,
                "jump left the newest row offscreen: {last:?}, {viewport:?}"
            );
        } else {
            tree.expect_key("feed-new-messages");
            assert!(
                (tree.bounds("thread-1").y - before.y).abs() <= 2.,
                "incoming row moved the reading position"
            );
            assert!(
                last.y >= viewport.bottom(),
                "feed took the reader away from history"
            );
        }
    }
}
