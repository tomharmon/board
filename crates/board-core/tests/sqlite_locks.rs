#![cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]

use anyhow::{Result, ensure};
use board_local::Board;
use rusqlite::Connection;
use std::{fs::OpenOptions, os::unix::fs::PermissionsExt, path::Path, process::Command};

const CHILD_PATH: &str = "BOARD_TEST_SQLITE_LOCK_CHILD";
const CASE: &str = "an_extra_board_open_keeps_the_kernel_lock_and_live_wal";

// F_SETLK is process-associated. A probe in the writer's own process would
// succeed even on the correct implementation; use a fresh process for each test.
fn check_external_lock(path: &Path, post: bool) -> Result<()> {
    let output = Command::new(std::env::current_exe()?)
        .args(["--exact", CASE])
        .env(CHILD_PATH, path)
        .env(
            "BOARD_TEST_POST_AFTER_LOCK",
            if post { "yes" } else { "no" },
        )
        .output()?;
    ensure!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn an_extra_board_open_keeps_the_kernel_lock_and_live_wal() -> Result<()> {
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        use rustix::{
            fs::{FlockOperation, fcntl_lock},
            io::Errno,
        };
        // Check the kernel's SQLite lock on the main inode, not merely whether
        // the separate WAL shared-memory lock happens to reject another writer.
        let fd = OpenOptions::new().write(true).open(&path)?;
        let lock = fcntl_lock(&fd, FlockOperation::NonBlockingLockExclusive);
        drop(fd);
        ensure!(
            matches!(lock, Err(Errno::ACCESS | Errno::AGAIN)),
            "expected parent's live SQLite read lock; got {lock:?}"
        );
        if std::env::var("BOARD_TEST_POST_AFTER_LOCK")?.as_str() == "yes" {
            Board::open(path)?.post(
                "shared",
                "child",
                "published by another process",
                &Default::default(),
            )?;
        }
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("board.sqlite");
    let board = Board::open(&path)?;
    assert_eq!(path.metadata()?.permissions().mode() & 0o777, 0o600);
    board.post("shared", "parent", "first", &Default::default())?;
    let mut connection = Connection::open(&path)?;
    let read = connection.transaction()?;
    let count =
        |c: &Connection| c.query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0));
    assert_eq!(count(&read)?, 1);
    check_external_lock(&path, false)?;
    drop(Board::open(&path)?);
    check_external_lock(&path, true)?;
    assert_eq!(count(&read)?, 1, "the pinned snapshot changed");
    read.commit()?;
    assert_eq!(count(&connection)?, 2);
    assert_eq!(
        board
            .messages(&Default::default(), 10)?
            .last()
            .unwrap()
            .text,
        "published by another process"
    );
    Ok(())
}

#[test]
fn dangling_database_links_never_create_a_target_with_default_permissions() -> Result<()> {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir()?;
    let target = temp.path().join("target.sqlite");
    let link = temp.path().join("board.sqlite");
    symlink(&target, &link)?;
    assert!(Board::open(&link).is_err());
    assert!(!target.try_exists()?);
    let db = Board::open(&target)?;
    assert_eq!(target.metadata()?.permissions().mode() & 0o777, 0o600);
    let through_link =
        Board::open(&link)?.post("linked", "human", "through symlink", &Default::default())?;
    assert_eq!(db.get(through_link.id)?.text, "through symlink");
    Ok(())
}
