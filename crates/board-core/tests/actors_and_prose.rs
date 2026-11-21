use anyhow::Result;
use board_local::{Board, Draft, Filter, access::Rules};
use serde_json::json;

fn mentioned(board: &Board, term: &str) -> Result<Vec<i64>> {
    Ok(board
        .messages(
            &Filter {
                mentions: Some(term.to_owned()),
                ..Default::default()
            },
            100,
        )?
        .into_iter()
        .map(|m| m.id)
        .collect())
}

#[test]
fn lexical_mentions_mask_mid_paragraph_fences_and_migrate_prior_indexes() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("v6.sqlite");
    let b = Board::open(&path)?;
    b.register_schema("memo", 1, "", &json!({"type":"object"}))?;
    let text = "Pair ```inline\n@[hidden] @hidden end```, then @visible.\n~~~json#memo@1\n{\"text\":\"@private\"}\n~~~";
    let m = b.post("dev", "ada", text, &Default::default())?;
    assert_eq!(mentioned(&b, "visible")?, [m.id]);
    for who in ["hidden", "private"] {
        assert!(mentioned(&b, who)?.is_empty());
    }
    let cursor = b.cursor()?;
    drop(b);
    let c = rusqlite::Connection::open(&path)?;
    c.execute("INSERT INTO mentions VALUES(?,'hidden')", [m.id])?;
    c.execute("INSERT INTO exact_mentions VALUES(?,'hidden')", [m.id])?;
    c.pragma_update(None, "user_version", 6)?;
    drop(c);
    let b = Board::open(&path)?;
    assert_eq!(b.cursor()?, cursor);
    assert_eq!(b.get(m.id)?.text, text);
    assert_eq!(mentioned(&b, "visible")?, [m.id]);
    assert!(mentioned(&b, "hidden")?.is_empty());
    Ok(())
}

#[test]
fn prose_tokens_exact_actor_directory_visibility_and_aliases_respect_scope() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("actors.sqlite");
    let b = Board::open(&path)?;
    b.set_grant("admin", None, "viewer", &Rules::parse("dev=read")?)?;
    b.set_actor_profile("sam", "dispatch", "Sam Coordinator")?;
    b.post("private", "sam", "private author", &Default::default())?;
    b.post(
        "private",
        "hidden",
        "do not enumerate me",
        &Default::default(),
    )?;
    let first=b.post("dev","ann/ci","@DISPATCH: please check. `@nope` code, user@server.test, inline fence ```example\n@ignore\n```\nNot @slack:USER.\n@[Case:UPPER] @[sam]",&Default::default())?;
    assert_eq!(mentioned(&b, "dispatch")?, [first.id]);
    assert_eq!(mentioned(&b, "sam")?, [first.id]);
    assert_eq!(mentioned(&b, "Case:UPPER")?, [first.id]);
    for bad in ["case:upper", "nope", "server", "ignore", "slack"] {
        assert!(mentioned(&b, bad)?.is_empty());
    }
    let second = b.post(
        "dev",
        "ann/ci",
        "bare handle @Dispatch",
        &Default::default(),
    )?;
    let mut viewer = Board::open(&path)?;
    viewer.bind_token(&b.issue_credential("admin", "viewer", None, None)?.token)?;
    assert_eq!(
        mentioned(&b, "sam")?,
        [first.id, second.id],
        "a reader of every channel may use a directory alias"
    );
    assert_eq!(
        mentioned(&viewer, "sam")?,
        [first.id, second.id],
        "both writers' @dispatch mentions were stored using the resolved ID"
    );
    assert!(
        mentioned(&viewer, "dispatch")?.is_empty(),
        "the scoped reader cannot expand an alias through the global directory"
    );
    let directory = viewer.actor_directory(None)?;
    assert!(
        directory
            .iter()
            .any(|a| a.id == "ann/ci" && a.kind == "agent" && a.via.as_deref() == Some("ann"))
    );
    assert!(
        directory
            .iter()
            .any(|a| a.id == "sam" && a.name == "Sam Coordinator")
    );
    assert!(directory.iter().any(|a| a.id == "viewer"));
    assert!(
        !directory
            .iter()
            .any(|a| a.id == "hidden" || a.id == "Case:UPPER"),
        "a typed unknown bracket is not an identity registration"
    );
    b.edit(first.id, "ann", "formerly known", None)?;
    assert!(viewer.actor_directory(None)?.iter().any(|a| a.id == "sam"));
    let words = Filter {
        mentions: Some("sam".into()),
        ..Default::default()
    };
    assert!(
        b.event_tail(100, &words)?
            .1
            .iter()
            .any(|e| e.message.as_ref().is_some_and(|m| m.id == second.id))
    );
    assert!(!viewer.event_tail(100, &words)?.1.is_empty());
    b.edit(second.id, "ann", "formerly mentioned here too", None)?;
    assert!(!viewer.actor_directory(None)?.iter().any(|a| a.id == "sam"));
    b.set_grant("admin", None, "blind", &Rules::parse("dev=post")?)?;
    let mut blind = Board::open(&path)?;
    blind.bind_token(&b.issue_credential("admin", "blind", None, None)?.token)?;
    let result = blind.post(
        "dev",
        "blind",
        "@[secret] and @[blind], code `@[not-prose]`",
        &board_local::PostOptions {
            key: Some("retry".into()),
            ..Default::default()
        },
    )?;
    assert_eq!(result.text, "@… and @[blind], code `@[not-prose]`");
    assert_eq!(blind.posted("blind", "retry")?, result);
    Ok(())
}

#[test]
fn checked_data_fences_include_html_prose_not_container_examples_and_must_close() -> Result<()> {
    let b = Board::memory()?;
    b.register_schema_as(
        "note",
        1,
        "",
        &json!({"type":"object","properties":{"data":true,"never":false},"$schema":"display-only"}),
        "ann",
    )?;
    let included = "<details>\n````json#note@1 optional-info\n{\"data\":2}\n````\n</details>";
    let m = b.post("dev", "ann", included, &Default::default())?;
    assert_eq!(m.blocks.len(), 1);
    for bad in [
        "```json#note@1\n{}",
        "<section>\n~~~json#note@1\n[]\n~~~\n</section>",
        "````json#note@1\n{}\n```",
    ] {
        assert!(b.edit(m.id, "ann", bad, None).is_err(), "{bad}");
    }
    let quoted = "> ```json#not-a-schema\n> no JSON\n> ```\n\n- an example\n\n  ```json#no@1\n  nope\n  ```\n\n~~~markdown\n```json#no@1\n~~~";
    assert!(
        b.post("dev", "ann", quoted, &Default::default())?
            .blocks
            .is_empty()
    );
    assert!(
        b.register_schema(
            "huge",
            1,
            "",
            &json!({"type":"object","enum":(0..600).collect::<Vec<_>>()})
        )
        .is_err()
    );
    assert!(
        b.register_schema(
            "invalid",
            1,
            "",
            &json!({"type":"object","const":{"s":"a\0b"}})
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn drafts_survive_restart_but_cannot_cross_an_unreadable_thread_or_actor() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("draft.db");
    let b = Board::open(&path)?;
    let root = b.post("dev", "ann", "here", &Default::default())?;
    let hidden = b.post("private", "ann", "elsewhere", &Default::default())?;
    let scope = format!("thread:{}", root.id);
    let body = Draft {
        body: "half-written".into(),
        channel: "dev".into(),
        reply_to: Some(root.id),
    };
    let g = b.set_grant("ann", None, "ann/bot", &Rules::parse("dev=read,post")?)?;
    let token = b.issue_credential("ann", "ann/bot", None, None)?.token;
    let mut client = Board::open(&path)?;
    client.bind_token(&token)?;
    let before = b.cursor()?;
    client.save_draft("ann/bot", &scope, Some(&body))?;
    assert_eq!(b.cursor()?, before);
    drop(client);
    let mut client = Board::open(&path)?;
    client.bind_token(&token)?;
    assert_eq!(client.load_draft("ann/bot", &scope)?, Some(body.clone()));
    assert!(client.load_draft("ann", &scope).is_err());
    assert!(
        client
            .save_draft(
                "ann/bot",
                &scope,
                Some(&Draft {
                    reply_to: Some(hidden.id),
                    ..body
                })
            )
            .is_err()
    );
    b.set_grant(
        "ann",
        Some(g.id),
        "ann/bot",
        &Rules::parse("elsewhere=read")?,
    )?;
    assert!(client.load_draft("ann/bot", &scope).is_err());
    assert!(client.save_draft("ann/bot", &scope, None).is_err());
    Ok(())
}
