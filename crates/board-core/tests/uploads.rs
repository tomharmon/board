use anyhow::Result;
use board_local::{Board, PostOptions, access::Rules};
use sha2::{Digest, Sha256};

#[test]
fn all_channel_read_does_not_make_unpublished_or_deleted_uploads_public() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("all-reads.db");
    let owner = Board::open(&path)?;
    owner.set_grant("admin", None, "guest", &Rules::parse("*=read,post")?)?;
    let mut guest = Board::open(&path)?;
    guest.bind_token(&owner.issue_credential("admin", "guest", None, None)?.token)?;
    let file = owner.upload("writer", "reports", "draft.txt", "text/plain", b"unsent")?;
    let opts = PostOptions {
        attachment_ids: vec![file.id],
        ..Default::default()
    };
    assert!(guest.download(file.id).is_err());
    assert!(guest.file_id("guest", &file.key).is_err());
    assert!(guest.post("other", "guest", "unpublished", &opts).is_err());
    let shared = owner.post("reports", "writer", "readable", &opts)?;
    assert_eq!(guest.download(file.id)?.1, b"unsent");
    owner.delete(shared.id, "writer", None)?;
    assert!(guest.download(file.id).is_err());
    Ok(())
}

#[test]
fn immutable_upload_permissions_follow_live_placements_and_uploader_origin() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("assets.db");
    let owner = Board::open(&path)?;
    owner.set_grant(
        "admin",
        None,
        "builder",
        &Rules::parse("internal=post;pub=post")?,
    )?;
    owner.set_grant("admin", None, "reader", &Rules::parse("pub=read;out=post")?)?;
    let builder_key = owner
        .issue_credential("admin", "builder/ci", None, None)?
        .token;
    let mut builder = Board::open(&path)?;
    builder.bind_token(&builder_key)?;
    let mut reader = Board::open(&path)?;
    reader.bind_token(&owner.issue_credential("admin", "reader", None, None)?.token)?;
    let file = builder.upload(
        "builder/ci",
        "internal",
        "build.png",
        "image/png",
        &[0, 255, 0],
    )?;
    assert_eq!(file.key.len(), 32);
    assert_eq!(file.content_type, "image/png");
    assert!(reader.file_id("reader", &file.key).is_err());
    assert!(reader.download(file.id).is_err());
    let opts = PostOptions {
        attachment_ids: vec![file.id],
        ..Default::default()
    };
    let before = owner.cursor()?;
    assert!(
        reader
            .post("out", "reader", "cannot claim a guessed file", &opts)
            .is_err()
    );
    assert_eq!(owner.cursor()?, before);
    let message = builder.post("pub", "builder/ci", "", &opts)?;
    assert_eq!(message.attachments.as_slice(), std::slice::from_ref(&file));
    assert_eq!(reader.file_id("reader", &file.key)?, file.id);
    assert_eq!(reader.download(file.id)?.1, [0, 255, 0]);
    let other = reader.post("out", "reader", "one shared asset", &opts)?;
    assert_eq!(other.attachments[0].id, file.id);
    owner.delete(message.id, "builder", None)?;
    assert!(
        reader.download(file.id).is_err(),
        "reader cannot read the remaining write-only placement"
    );
    assert_eq!(
        builder.download(file.id)?.1,
        [0, 255, 0],
        "uploader can still read its own file"
    );
    let narrow = owner.issue_credential(
        "admin",
        "builder/ci",
        Some(&Rules::parse("pub=post")?),
        None,
    )?;
    let mut limited = Board::open(path)?;
    limited.bind_token(&narrow.token)?;
    assert!(
        limited.download(file.id).is_err(),
        "a different cap on the same actor may not read private origin bytes"
    );
    Ok(())
}

#[test]
fn staged_parts_resume_after_restart_validate_bytes_and_finish_once() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("direct.db");
    let b = Board::open(&path)?;
    let full = vec![42; board_local::MAX_ATTACHMENT_BYTES + 19];
    let cut = board_local::MAX_ATTACHMENT_BYTES;
    let sha = format!("{:x}", Sha256::digest(&full));
    let ticket = b.start_upload(
        "ann",
        "dev",
        "recording.webm",
        "video/webm",
        full.len() as i64,
        Some(&sha),
    )?;
    assert!(b.upload_part("bob", &ticket.key, 0, &full[..cut]).is_err());
    assert!(b.upload_part("ann", &ticket.key, 12, &full[..cut]).is_err());
    b.upload_part("ann", &ticket.key, 0, &full[..cut])?;
    assert!(b.finish_upload("ann", &ticket.key).is_err());
    drop(b);
    let b = Board::open(&path)?;
    let head = b.cursor()?;
    let resume = b.upload_status("ann", &ticket.key)?;
    assert_eq!(resume.received, cut as i64);
    b.upload_part("ann", &ticket.key, 0, &full[..cut])?;
    assert!(b.upload_part("ann", &ticket.key, 0, b"changed").is_err());
    assert_eq!(b.cursor()?, head);
    b.upload_part("ann", &ticket.key, resume.received, &full[cut..])?;
    let (file, made) = b.finish_upload("ann", &ticket.key)?;
    assert!(made);
    let head = b.cursor()?;
    assert_eq!(b.finish_upload("ann", &ticket.key)?, (file.clone(), false));
    assert_eq!(b.cursor()?, head);
    assert_eq!(b.download(file.id)?.1, full);
    let mismatch = b.start_upload(
        "ann",
        "dev",
        "bad.bin",
        "application/octet-stream",
        3,
        Some(&"0".repeat(64)),
    )?;
    b.upload_part("ann", &mismatch.key, 0, b"abc")?;
    let head = b.cursor()?;
    assert!(b.finish_upload("ann", &mismatch.key).is_err());
    assert_eq!(b.cursor()?, head);
    assert_eq!(b.upload_status("ann", &mismatch.key)?.asset, None);
    assert!(
        b.start_upload(
            "ann",
            "dev",
            "bad",
            "text/plain",
            board_local::MAX_DIRECT_BYTES + 1,
            None
        )
        .is_err()
    );
    for name in [".env", "../name", "spoof\u{202e}.png", "line\u{2029}name"] {
        assert!(b.upload("ann", "dev", name, "text/plain", b"").is_err());
    }
    Ok(())
}
