use board_local::{Board, errors::classify};
use std::{
    sync::{Arc, Barrier},
    thread,
};

#[test]
fn private_channel_race_grants_exactly_one_family_and_rollback_leaves_no_policy()
-> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("race.db");
    let board = Board::open(&path)?;
    let all = serde_json::from_str(r#"{"*":"all"}"#)?;
    let mut contenders = Vec::new();
    for who in ["ada", "bea"] {
        board.set_grant("admin", None, who, &all)?;
        contenders.push((who, board.issue_credential("admin", who, None, None)?.token));
    }
    let old = board.cursor()?;
    for invalid in ["*", "*/desk", "source:external/person"] {
        assert_eq!(
            classify(
                &board
                    .create_private_channel("confidential", invalid)
                    .unwrap_err()
            )
            .1,
            "no_own_principal"
        );
    }
    let sql = rusqlite::Connection::open(&path)?;
    sql.execute_batch(
        "CREATE TRIGGER fail_create BEFORE INSERT ON channels WHEN NEW.name='confidential'
        BEGIN SELECT raise(ABORT,'injected after both grants'); END;",
    )?;
    assert!(board.create_private_channel("confidential", "ada").is_err());
    assert_eq!(board.cursor()?, old);
    assert_eq!(board.grants()?.len(), 2);
    sql.execute_batch("DROP TRIGGER fail_create")?;
    drop(sql);

    let gate = Arc::new(Barrier::new(2));
    let workers: Vec<_> = contenders
        .into_iter()
        .map(|(who, token)| {
            let p = path.clone();
            let gate = Arc::clone(&gate);
            thread::spawn(move || -> anyhow::Result<_> {
                let mut peer = Board::open(p)?;
                peer.bind_token(&token)?;
                gate.wait();
                let made = peer.create_private_channel("confidential", who);
                Ok((
                    who,
                    made.map(|grants| grants.len()).map_err(|e| classify(&e).1),
                ))
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect::<anyhow::Result<_>>()?;
    let winner = results.iter().find(|(_, r)| *r == Ok(2)).unwrap().0;
    let loser = results.iter().find(|(_, r)| r.is_err()).unwrap().0;
    assert_ne!(winner, loser);
    assert_eq!(board.cursor()?, old + 3);
    assert_eq!(board.grants()?.len(), 4);
    use board_local::access::Action;
    assert!(
        board
            .explain(&format!("{winner}/desk"), "confidential/deeper")?
            .contains(&Action::Manage)
    );
    assert!(board.explain(loser, "confidential/deeper")?.is_empty());
    assert!(
        board
            .explain(loser, "confidential-next")?
            .contains(&Action::Manage)
    );
    Ok(())
}
