use anyhow::Result;
use board_local::{
    Board,
    access::{Action, Rules},
    errors::classify,
};

#[test]
fn most_specific_pattern_then_closest_local_subject_then_union_at_a_tie() -> Result<()> {
    let d = tempfile::tempdir()?;
    let path = d.path().join("acl.db");
    let b = Board::open(&path)?;
    let lock = b.set_grant(
        "root",
        None,
        "*",
        &Rules::parse("ops/closed/=none;open/=read")?,
    )?;
    let bearer = b.issue_credential("root", "worker/a", None, None)?.token;
    assert_eq!(
        classify(&Board::open(&path)?.bind_token(&bearer).unwrap_err()).1,
        "no_access"
    );
    assert!(b.explain("stranger", "open/x")?.is_empty());
    b.set_grant("root", None, "worker", &Rules::parse("ops/=all")?)?;
    let shut = b.explain_decision("worker/a", "ops/closed/x")?;
    assert!(shut.actions.is_empty());
    assert_eq!(shut.grants, [lock.id]);
    assert_eq!(shut.level.as_deref(), Some("everyone"));
    assert_eq!(shut.over, ["ops/"]);
    let mut agent = Board::open(&path)?;
    agent.bind_token(&bearer)?;
    assert!(
        agent
            .post("ops/closed/x", "worker/a", "no", &Default::default())
            .is_err()
    );
    assert_eq!(b.explain("worker/a", "open/x")?, [Action::Read]);
    let family = b.set_grant("root", None, "worker", &Rules::parse("ops/closed/=all")?)?;
    let g1 = b.set_group_grant(
        "root",
        None,
        &["worker/a".into(), "backup".into()],
        &Rules::parse("ops/closed/=read")?,
        false,
    )?;
    let g2 = b.set_grant("root", None, "worker/a", &Rules::parse("ops/closed/=post")?)?;
    let open = b.explain_decision("worker/a", "ops/closed/x")?;
    assert_eq!(open.level.as_deref(), Some("principal"));
    assert_eq!(open.actions, [Action::Read, Action::Post]);
    assert_eq!(open.grants, [g1.id, g2.id]);
    assert_eq!(
        b.explain_decision("worker/b", "ops/closed/x")?.grants,
        [family.id]
    );
    agent.post("ops/closed/x", "worker/a", "yes", &Default::default())?;
    assert_eq!(
        b.patch_grant(
            "root",
            g1.id,
            &["new".into()],
            &["backup".into()],
            None,
            false
        )?
        .subjects,
        ["worker/a", "new"]
    );
    assert_eq!(
        classify(
            &b.patch_grant("root", g1.id, &[], &["backup".into()], None, false)
                .unwrap_err()
        )
        .1,
        "not_in_grant"
    );
    assert!(b.revoke_grant_with_outcome("root", g2.id, false)?);
    let head = b.cursor()?;
    assert!(!b.revoke_grant_with_outcome("root", g2.id, false)?);
    assert_eq!(b.cursor()?, head);
    Ok(())
}

#[test]
fn scoped_manage_checks_reach_under_locks_admission_and_prospective_lockout() -> Result<()> {
    let d = tempfile::tempdir()?;
    let path = d.path().join("policy.db");
    let mut b = Board::open(&path)?;
    b.set_grant(
        "root",
        None,
        "lead",
        &Rules::parse("ops/=manage;ops/private/=none;outside/=none")?,
    )?;
    b.set_grant("root", None, "*", &Rules::parse("ops/private/=none")?)?;
    let token = b.issue_credential("root", "lead", None, None)?.token;
    let mut lead = Board::open(&path)?;
    lead.bind_token(&token)?;
    let denied = lead
        .set_grant("lead", None, "other", &Rules::parse("ops/=read")?)
        .unwrap_err();
    assert_eq!(classify(&denied).1, "grant_reach");
    assert!(lead.grants().is_err() && lead.credentials().is_err());
    let g = lead.set_grant("lead", None, "other", &Rules::parse("ops/ci/=post")?)?;
    assert!(
        lead.set_grant(
            "lead",
            Some(g.id),
            "other",
            &Rules::parse("elsewhere/=read")?
        )
        .is_err()
    );
    let secret = b.set_grant("root", None, "*", &Rules::parse("outside/=read")?)?;
    let head = b.cursor()?;
    assert_eq!(
        classify(
            &lead
                .set_grant("lead", None, "new", &Rules::parse("ops/ci/=post")?)
                .unwrap_err()
        )
        .1,
        "grant_admission"
    );
    assert_eq!(b.cursor()?, head);
    assert!(lead.revoke_grant_with_outcome("lead", g.id, false)?);
    b.revoke_grant("root", secret.id)?;
    let protected = Rules::parse("ops/ci/=none")?;
    let before = b.cursor()?;
    assert_eq!(
        classify(&lead.set_grant("lead", None, "*", &protected).unwrap_err()).1,
        "grant_lockout"
    );
    assert_eq!(b.cursor()?, before);
    let lock = lead.set_group_grant("lead", None, &["*".into()], &protected, true)?;
    assert!(lead.revoke_grant("lead", lock.id).is_err());
    b.revoke_grant("root", lock.id)?;
    b.with_request(
        Some(&token),
        "root",
        Some("ops/ci/=manage;ops/ci/private/=none"),
        |scoped, who| {
            let e = scoped
                .set_grant(who, None, "new", &Rules::parse("ops/ci/=post")?)
                .unwrap_err();
            assert_eq!(classify(&e).1, "grant_reach");
            Ok(())
        },
    )?;
    Ok(())
}

#[test]
fn inherited_everyone_lock_still_constrains_a_new_member_and_global_registry_checks_wildcard_manage()
-> Result<()> {
    let d = tempfile::tempdir()?;
    let path = d.path().join("registry.db");
    let b = Board::open(&path)?;
    b.set_grant("root", None, "lead", &Rules::parse("ops/=manage")?)?;
    b.set_grant("root", None, "*", &Rules::parse("ops/private/=none")?)?;
    let mut lead = Board::open(&path)?;
    lead.bind_token(&b.issue_credential("root", "lead", None, None)?.token)?;
    lead.set_grant("lead", None, "new", &Rules::parse("ops/=read")?)?;
    assert_eq!(b.explain("new", "ops/home")?, [Action::Read]);
    assert!(b.explain("new", "ops/private/home")?.is_empty());
    let schema = serde_json::json!({"type":"object"});
    assert!(
        lead.register_schema_as("regional", 1, "", &schema, "lead")
            .is_err()
    );
    let (batch, created) = lead.group_grant_batch(
        "lead",
        &[
            (vec!["*".into()], Rules::parse("ops/shared/=none")?),
            (vec!["lead".into()], Rules::parse("ops/shared/=manage")?),
        ],
        Some("keep-maker"),
        false,
    )?;
    assert!(created);
    assert_eq!(lead.grant_batch(batch.id)?, batch);
    assert!(b.explain("new", "ops/shared")?.is_empty());
    assert!(b.explain("lead", "ops/shared")?.contains(&Action::Manage));
    b.set_grant(
        "root",
        None,
        "registrar",
        &Rules::parse("*=manage;ops/internal/=none")?,
    )?;
    let mut registrar = Board::open(&path)?;
    registrar.bind_token(&b.issue_credential("root", "registrar", None, None)?.token)?;
    registrar.register_schema_as("global", 1, "", &schema, "registrar")?;
    assert_eq!(lead.schema("global", 1)?.by.as_deref(), Some("registrar"));
    assert!(
        registrar
            .post(
                "ops/internal/hidden",
                "registrar",
                "denied",
                &Default::default()
            )
            .is_err()
    );
    Ok(())
}
