//! Immutable uploaded bytes and the messages that carry them. Staging is private
//! to its writer; readers are authorized by live placements, not by knowing an ID.
use crate::{
    Attachment, Board, MAX_ATTACHMENT_BYTES, MAX_ATTACHMENTS, PostOptions,
    access::{Access, Action},
    errors::Kind::{Conflict, Invalid, Missing},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;

pub const MAX_DIRECT_BYTES: i64 = 100 * 1024 * 1024;
pub const FILE_CHUNK_BYTES: usize = 64 * 1024;
pub(crate) fn binary_type() -> String {
    "application/octet-stream".into()
}

pub(crate) fn init(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS file_assets(id INTEGER PRIMARY KEY AUTOINCREMENT,key TEXT NOT NULL UNIQUE,
        actor TEXT NOT NULL,origin TEXT NOT NULL,name TEXT NOT NULL,content_type TEXT NOT NULL,size INTEGER NOT NULL,bytes BLOB NOT NULL);
      CREATE TABLE IF NOT EXISTS message_files(message_id INTEGER NOT NULL REFERENCES messages(id),position INTEGER NOT NULL,
        asset INTEGER NOT NULL REFERENCES file_assets(id),PRIMARY KEY(message_id,position));
      CREATE INDEX IF NOT EXISTS file_placements ON message_files(asset,message_id);
      CREATE TABLE IF NOT EXISTS pending_uploads(key TEXT PRIMARY KEY,actor TEXT NOT NULL,channel TEXT NOT NULL,name TEXT NOT NULL,
        content_type TEXT NOT NULL,size INTEGER NOT NULL,received INTEGER NOT NULL DEFAULT 0,sha256 TEXT,expires INTEGER NOT NULL,asset INTEGER REFERENCES file_assets(id));
      CREATE TABLE IF NOT EXISTS upload_parts(key TEXT NOT NULL REFERENCES pending_uploads(key) ON DELETE CASCADE,offset INTEGER NOT NULL,bytes BLOB NOT NULL,PRIMARY KEY(key,offset));")?;
    Ok(())
}
fn meta(r: &rusqlite::Row<'_>) -> rusqlite::Result<Attachment> {
    Ok(Attachment {
        id: r.get(0)?,
        key: r.get(1)?,
        name: r.get(2)?,
        size: r.get(3)?,
        content_type: r.get(4)?,
    })
}
pub(crate) fn on_message(c: &Connection, id: i64) -> Result<Vec<Attachment>> {
    Ok(c.prepare("SELECT a.id,a.key,a.name,a.size,a.content_type FROM message_files f JOIN file_assets a ON a.id=f.asset WHERE f.message_id=? ORDER BY f.position")?
        .query_map([id],meta)?.collect::<rusqlite::Result<_>>()?)
}
pub(crate) fn valid_file(name: &str, mime: &str) -> Result<()> {
    valid_name(name)?;
    ensure!(
        mime.len() <= 127
            && mime
                .split_once('/')
                .is_some_and(|(t, s)| !t.is_empty() && !s.is_empty() && !s.contains('/'))
            && mime
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$&^_.+-/".contains(&b)),
        Invalid.error("invalid_media_type", "expected media type/subtype")
    );
    Ok(())
}
pub fn valid_name(name: &str) -> Result<()> {
    static INVISIBLE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"[\p{Cf}\x{2028}\x{2029}]").expect("constant filename pattern")
    });
    ensure!(
        !name.is_empty()
            && name.trim() == name
            && name.len() <= 255
            && !name.contains(['/', '\\'])
            && !name.starts_with('.')
            && !name.chars().any(char::is_control)
            && !INVISIBLE.is_match(name),
        Invalid.error(
            "invalid_attachment",
            "expected a visible filename without a leading dot, separators or controls"
        )
    );
    Ok(())
}
pub fn content_type_for(name: &str) -> &'static str {
    match name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpeg" | "jpg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("avif") => "image/avif",
        Some("svg") => "image/svg+xml",
        Some("txt" | "log") => "text/plain",
        Some("md") => "text/markdown",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("pdf") => "application/pdf",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mov") => "video/quicktime",
        Some("html") => "text/html",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}
pub(crate) fn add(
    c: &Connection,
    actor: &str,
    channel: &str,
    name: &str,
    kind: &str,
    size: i64,
    data: impl rusqlite::ToSql,
) -> Result<i64> {
    c.execute("INSERT INTO file_assets(key,actor,origin,name,content_type,size,bytes) VALUES(lower(hex(randomblob(16))),?,?,?,?,?,?)",params![actor,channel,name,kind,size,data])?;
    Ok(c.last_insert_rowid())
}
pub(crate) fn attach(
    tx: &Transaction<'_>,
    message: i64,
    actor: &str,
    channel: &str,
    opts: &PostOptions,
) -> Result<()> {
    for (pos, id) in opts.attachment_ids.iter().enumerate() {
        Board::attachment_on(tx, *id)?;
        tx.execute(
            "INSERT INTO message_files VALUES(?,?,?)",
            params![message, pos as i64, id],
        )?;
    }
    for (i, (name, data)) in opts.files.iter().enumerate() {
        let id = add(
            tx,
            actor,
            channel,
            name,
            content_type_for(name),
            data.len() as i64,
            data,
        )?;
        tx.execute(
            "INSERT INTO message_files VALUES(?,?,?)",
            params![message, (i + opts.attachment_ids.len()) as i64, id],
        )?;
    }
    Ok(())
}
fn missing() -> crate::errors::Rejection {
    Missing.error("attachment_not_found", "attachment not found")
}
impl Access {
    pub(crate) fn files(&self, c: &Connection, actor: &str, ids: &[i64]) -> Result<()> {
        self.actor(actor)?;
        ensure!(ids.len() <= MAX_ATTACHMENTS, "too many attachments");
        let principal = actor.split('/').next().unwrap_or(actor);
        for id in ids {
            let (uploader, origin): (String, String) = c
                .query_row(
                    "SELECT actor,origin FROM file_assets WHERE id=?",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
                .context(missing())?;
            // Full channel visibility is not ownership of other people's unpublished uploads.
            // Numeric compatibility IDs are enumerable; every delegate needs a live placement.
            if self.is_trusted()
                || (uploader.split('/').next() == Some(principal)
                    && [Action::Post, Action::Read, Action::Sync]
                        .into_iter()
                        .any(|p| self.allows(p, &origin)))
            {
                continue;
            }
            let channels=c.prepare("SELECT DISTINCT m.channel FROM message_files f JOIN messages m ON m.id=f.message_id WHERE f.asset=? AND NOT m.deleted")?.query_map([id],|r|r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
            ensure!(
                channels.iter().any(|s| self.allows(Action::Read, s)),
                missing()
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Upload {
    pub key: String,
    pub channel: String,
    pub name: String,
    pub content_type: String,
    pub size: i64,
    pub received: i64,
    pub sha256: Option<String>,
    pub expires: i64,
    pub asset: Option<i64>,
}

fn session(c: &Connection, key: &str, actor: &str) -> Result<Upload> {
    c.query_row("SELECT key,channel,name,content_type,size,received,sha256,expires,asset FROM pending_uploads WHERE key=? AND actor=? AND (asset IS NOT NULL OR expires>unixepoch())",
        [key,actor],|r|Ok(Upload{key:r.get(0)?,channel:r.get(1)?,name:r.get(2)?,content_type:r.get(3)?,size:r.get(4)?,received:r.get(5)?,sha256:r.get(6)?,expires:r.get(7)?,asset:r.get(8)?}))
        .optional()?.context(Missing.error("upload_not_found","upload not found"))
}
impl Board {
    pub(crate) fn authorize_files(&self, c: &Connection, actor: &str, ids: &[i64]) -> Result<()> {
        self.access_on(c)?.files(c, actor, ids)
    }
    pub(crate) fn attachment_on(c: &Connection, id: i64) -> Result<Attachment> {
        c.query_row(
            "SELECT id,key,name,size,content_type FROM file_assets WHERE id=?",
            [id],
            meta,
        )
        .optional()?
        .context(missing())
    }
    pub(crate) fn file_bytes_on(c: &Connection, id: i64) -> Result<Vec<u8>> {
        c.query_row("SELECT bytes FROM file_assets WHERE id=?", [id], |r| {
            r.get(0)
        })
        .optional()?
        .context(missing())
    }
    pub fn file_id(&self, actor: &str, key: &str) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        let id = if key.len() == 32 {
            tx.query_row("SELECT id FROM file_assets WHERE key=?", [key], |r| {
                r.get(0)
            })
            .optional()?
            .context(missing())?
        } else {
            key.parse::<i64>().context(missing())?
        };
        self.authorize_files(&tx, actor, &[id])?;
        tx.commit()?;
        Ok(id)
    }
    pub fn download(&self, id: i64) -> Result<(Attachment, Vec<u8>)> {
        let a = self.access_on(&self.conn)?;
        let fallback = crate::local::actor_name();
        self.download_as(a.bound_actor().unwrap_or(&fallback), id)
    }
    pub fn download_as(&self, actor: &str, id: i64) -> Result<(Attachment, Vec<u8>)> {
        let tx = self.conn.unchecked_transaction()?;
        self.authorize_files(&tx, actor, &[id])?;
        let out = (Self::attachment_on(&tx, id)?, Self::file_bytes_on(&tx, id)?);
        tx.commit()?;
        Ok(out)
    }
    /// Read only this slice, authorizing the live placement in the same snapshot.
    pub fn download_range_as(
        &self,
        actor: &str,
        id: i64,
        offset: i64,
        length: usize,
    ) -> Result<(Attachment, Vec<u8>)> {
        ensure!(
            offset >= 0 && (1..=FILE_CHUNK_BYTES).contains(&length),
            Invalid.error(
                "invalid_file_range",
                "expected a nonnegative offset and 1..65536 bytes"
            )
        );
        let tx = self.conn.unchecked_transaction()?;
        self.authorize_files(&tx, actor, &[id])?;
        let file = Self::attachment_on(&tx, id)?;
        ensure!(
            offset <= file.size,
            Invalid.error("invalid_file_range", "offset exceeds file size")
        );
        let mut data = vec![0; (file.size - offset).min(length as i64) as usize];
        if !data.is_empty() {
            let blob = tx.blob_open(rusqlite::MAIN_DB, "file_assets", "bytes", id, true)?;
            blob.read_at_exact(&mut data, usize::try_from(offset)?)?;
        }
        tx.commit()?;
        Ok((file, data))
    }
    pub fn upload(
        &self,
        actor: &str,
        channel: &str,
        name: &str,
        mime: &str,
        data: &[u8],
    ) -> Result<Attachment> {
        crate::validate_channel(channel)?;
        crate::validate_actor(actor)?;
        valid_file(name, mime)?;
        ensure!(
            data.len() <= MAX_ATTACHMENT_BYTES,
            "simple upload exceeds 4 MiB"
        );
        self.write(|tx| {
            let a = self.access_on(tx)?;
            a.actor(actor)?;
            a.require(Action::Post, channel)?;
            let id = add(tx, actor, channel, name, mime, data.len() as i64, data)?;
            Self::event_tx(
                tx,
                "attachment.uploaded",
                actor,
                None,
                serde_json::json!({"id":id}),
            )?;
            Self::attachment_on(tx, id)
        })
    }
    pub fn start_upload(
        &self,
        actor: &str,
        channel: &str,
        name: &str,
        mime: &str,
        size: i64,
        sha256: Option<&str>,
    ) -> Result<Upload> {
        crate::validate_actor(actor)?;
        crate::validate_channel(channel)?;
        valid_file(name, mime)?;
        ensure!(
            (0..=MAX_DIRECT_BYTES).contains(&size),
            "upload exceeds 100 MiB"
        );
        ensure!(
            sha256.is_none_or(|s| s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))),
            "expected lowercase SHA-256"
        );
        self.write(|tx|{let a=self.access_on(tx)?;a.actor(actor)?;a.require(Action::Post,channel)?;
            tx.execute("DELETE FROM pending_uploads WHERE asset IS NULL AND expires<=unixepoch()",[])?;
            let (own,all):(i64,i64)=tx.query_row("SELECT count(*) FILTER(WHERE actor=?),coalesce(sum(size),0) FROM pending_uploads WHERE asset IS NULL",[actor],|r|Ok((r.get(0)?,r.get(1)?)))?;
            ensure!(own<8 && all+size<=8*MAX_DIRECT_BYTES,Conflict.error("upload_quota","finish an existing upload first"));
            let key:String=tx.query_row("INSERT INTO pending_uploads(key,actor,channel,name,content_type,size,sha256,expires) VALUES(lower(hex(randomblob(16))),?,?,?,?,?,?,unixepoch()+3600) RETURNING key",params![actor,channel,name,mime,size,sha256],|r|r.get(0))?;
            Self::event_tx(tx,"upload.started",actor,None,serde_json::json!({"key":key}))?;
            session(tx,&key,actor)})
    }
    pub fn upload_status(&self, actor: &str, key: &str) -> Result<Upload> {
        let tx = self.conn.unchecked_transaction()?;
        let a = self.access_on(&tx)?;
        a.actor(actor)?;
        let s = session(&tx, key, actor)?;
        a.require(Action::Post, &s.channel)?;
        tx.commit()?;
        Ok(s)
    }
    pub fn upload_part(&self, actor: &str, key: &str, at: i64, data: &[u8]) -> Result<Upload> {
        ensure!(
            !data.is_empty() && data.len() <= MAX_ATTACHMENT_BYTES && at >= 0,
            "each part must contain 1 byte to 4 MiB at a nonnegative offset"
        );
        self.write(|tx|{let a=self.access_on(tx)?;a.actor(actor)?;let s=session(tx,key,actor)?;a.require(Action::Post,&s.channel)?;
            ensure!(s.asset.is_none(),Conflict.error("upload_complete","upload is already complete"));
            if at!=s.received{
                let same:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM upload_parts WHERE key=? AND offset=? AND bytes=?)",params![key,at,data],|r|r.get(0))?;
                ensure!(same,Conflict.error("upload_offset",format!("expected offset {}",s.received)));return Ok(s);
            }
            ensure!(s.received+data.len() as i64<=s.size,"received more bytes than declared");
            let count:i64=tx.query_row("SELECT count(*) FROM upload_parts WHERE key=?",[key],|r|r.get(0))?;
            ensure!(count<512,"too many upload parts");
            tx.execute("INSERT INTO upload_parts VALUES(?,?,?)",params![key,at,data])?;
            tx.execute("UPDATE pending_uploads SET received=received+? WHERE key=?",params![data.len() as i64,key])?;
            Self::event_tx(tx,"upload.progress",actor,None,serde_json::json!({"key":key,"received":s.received+data.len() as i64}))?;
            session(tx,key,actor)})
    }
    pub fn finish_upload(&self, actor: &str, key: &str) -> Result<(Attachment, bool)> {
        self.write(|tx| {
            let a = self.access_on(tx)?;
            a.actor(actor)?;
            let s = session(tx, key, actor)?;
            a.require(Action::Post, &s.channel)?;
            if let Some(id) = s.asset {
                return Ok((Self::attachment_on(tx, id)?, false));
            }
            ensure!(
                s.size == s.received,
                Conflict.error(
                    "upload_incomplete",
                    format!("received {} of {} bytes", s.received, s.size)
                )
            );
            let id = add(
                tx,
                actor,
                &s.channel,
                &s.name,
                &s.content_type,
                s.size,
                rusqlite::blob::ZeroBlob(s.size as i32),
            )?;
            let mut blob = tx.blob_open(rusqlite::MAIN_DB, "file_assets", "bytes", id, false)?;
            let mut hash = Sha256::new();
            let mut parts =
                tx.prepare("SELECT bytes FROM upload_parts WHERE key=? ORDER BY offset")?;
            for part in parts.query_map([key], |r| r.get::<_, Vec<u8>>(0))? {
                let data = part?;
                blob.write_all(&data)?;
                hash.update(&data);
            }
            blob.close()?;
            if let Some(expected) = s.sha256 {
                ensure!(
                    format!("{:x}", hash.finalize()) == expected,
                    Conflict.error("hash_mismatch", "uploaded bytes do not match SHA-256")
                );
            }
            tx.execute(
                "UPDATE pending_uploads SET asset=? WHERE key=?",
                params![id, key],
            )?;
            tx.execute("DELETE FROM upload_parts WHERE key=?", [key])?;
            Self::event_tx(
                tx,
                "attachment.uploaded",
                actor,
                None,
                serde_json::json!({"id":id}),
            )?;
            Ok((Self::attachment_on(tx, id)?, true))
        })
    }
}
