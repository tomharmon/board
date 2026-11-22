use anyhow::Result;
use board_local::{
    Board,
    access::Rules,
    automation::{self, TaskTarget},
};

#[test]
fn unsupported_boards_are_rejected_without_rewriting_them() -> Result<()> {
    let dir = tempfile::tempdir()?;
    for (app, version) in [(0x424f4152, 24), (0x424f4152, 999), (0, 0)] {
        let path = dir.path().join(format!("board-{version}.db"));
        let db = rusqlite::Connection::open(&path)?;
        db.execute_batch(
            "CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES('untouched')",
        )?;
        db.pragma_update(None, "application_id", app)?;
        db.pragma_update(None, "user_version", version)?;
        drop(db);
        let bytes = std::fs::read(&path)?;
        assert!(Board::open(&path).is_err());
        assert_eq!(std::fs::read(&path)?, bytes);
    }
    Ok(())
}

#[test]
fn sidebar_marks_survive_restart_without_acknowledging_newer_messages_or_other_identities()
-> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("board.db");
    let board = Board::open(&path)?;
    let first = board.post("desk", "host", "one", &Default::default())?;
    let later = board.reply(first.id, "host", "two", &Default::default())?;
    let hidden = board.post("other", "host", "not in desk", &Default::default())?;
    automation::create_task(&board, "host", TaskTarget::Existing(first.id), &[], None)?;
    automation::create_task(&board, "host", TaskTarget::Existing(hidden.id), &[], None)?;
    board.set_grant("host", None, "reader", &Rules::parse("desk=read")?)?;
    let secret = board.issue_credential("host", "reader", None, None)?.token;
    let head = board.cursor()?;
    let mut scoped = Board::open(&path)?;
    scoped.bind_token(&secret)?;
    assert_eq!(automation::open_task_count(&scoped)?, 1);
    assert_eq!(automation::open_task_count(&board)?, 2);
    scoped.mark_channels_read("reader", &[("desk", first.id)])?;
    assert_eq!(scoped.channel_read_marks("reader")?["desk"], first.id);
    assert_eq!(scoped.channel("desk")?.last, Some(later.id));
    assert!(
        scoped
            .mark_channels_read("reader", &[("desk", later.id), ("other", hidden.id)])
            .is_err()
    );
    assert!(
        scoped
            .mark_channels_read("reader", &[("desk", hidden.id)])
            .is_err()
    );
    assert!(scoped.channel_read_marks("another").is_err());
    drop(scoped);
    let mut scoped = Board::open(&path)?;
    scoped.bind_token(&secret)?;
    assert_eq!(scoped.channel_read_marks("reader")?["desk"], first.id);
    scoped.mark_channels_read("reader", &[("desk", later.id)])?;
    scoped.mark_channels_read("reader", &[("desk", first.id)])?;
    assert_eq!(scoped.channel_read_marks("reader")?["desk"], later.id);
    assert!(board.channel_read_marks("reader")?.is_empty());
    assert!(board.channel_read_marks("host")?.is_empty());
    assert_eq!(board.cursor()?, head);
    assert_eq!(scoped.activity_seen("reader")?, 0);
    Ok(())
}
