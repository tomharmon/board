use super::*;
use crate::{Attachment, files};

#[derive(Debug, Serialize)]
pub struct SourceFile {
    pub thread: String,
    pub external: String,
    pub message: i64,
    pub asset: i64,
    pub direction: Direction,
}
pub struct InboundFile<'a> {
    pub external: &'a str,
    pub name: &'a str,
    pub mime: &'a str,
    pub bytes: &'a [u8],
}
#[derive(Debug, Serialize)]
pub struct MediaRequest {
    pub thread: String,
    pub external: String,
    pub file: String,
    pub attempts: u32,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct FileNotice {
    pub name: String,
    pub size: Option<i64>,
    pub reason: String,
}
pub(crate) fn file_notices_on(c: &Connection, message: i64) -> Result<Vec<FileNotice>> {
    Ok(c.prepare("SELECT m.name,m.size,coalesce(q.error,'not downloaded') FROM sync_file_metadata m JOIN sync_media_fetch q USING(source,thread,external,file) WHERE m.message=? AND q.attempts>=3 ORDER BY m.rowid")?
        .query_map([message],|r|Ok(FileNotice{name:r.get(0)?,size:r.get(1)?,reason:r.get(2)?}))?.collect::<rusqlite::Result<_>>()?)
}

pub(super) fn queue_asset(
    tx: &Transaction<'_>,
    s: &Source,
    thread: &str,
    m: &Message,
    asset: i64,
    delete: bool,
) -> Result<i64> {
    let event = if delete { "file.delete" } else { "file.share" };
    let key = if delete {
        format!("unfile:{}:{asset}", m.id)
    } else {
        format!("file:{}:{asset}", m.id)
    };
    let d = Delivery {
        event: event.into(),
        message: m.id,
        cursor: m.version,
        parent: None,
        text: String::new(),
        emoji: None,
        asset: Some(asset),
    };
    tx.execute("INSERT INTO sync_queue(source,direction,thread,event_key,payload,raw) VALUES(?,'out',?,?,?,'null')
        ON CONFLICT(source,direction,thread,event_key) DO UPDATE SET state='pending',attempts=0,available=0,error=NULL
        WHERE sync_queue.state='skipped' AND sync_queue.error IN ('publishing disabled','file mirroring disabled')",params![s.name,thread,key,serde_json::to_string(&d)?])?;
    Ok(tx.query_row(
        "SELECT id FROM sync_queue WHERE source=? AND direction='out' AND thread=? AND event_key=?",
        params![s.name, thread, key],
        |r| r.get(0),
    )?)
}

impl Board {
    /// A local retry ledger. No provider bytes are fetched by core.
    pub fn pending_media(&self, who: &str, name: &str, failed: bool) -> Result<Vec<MediaRequest>> {
        let tx = self.conn.unchecked_transaction()?;
        self.authorized_source(&tx, name, who)?;
        let list=tx.prepare("SELECT q.thread,q.external,q.file,q.attempts,q.error FROM sync_media_fetch q JOIN sync_copies c ON c.source=q.source AND c.thread=q.thread AND c.external=q.external JOIN messages m ON m.id=c.message WHERE q.source=? AND m.deleted=0 AND (q.attempts>=3)=? AND NOT EXISTS(SELECT 1 FROM sync_file_copies f WHERE f.source=q.source AND f.thread=q.thread AND f.message=c.message AND f.external=q.file AND f.direction='in') ORDER BY q.attempts,q.rowid LIMIT 200")?
            .query_map(params![name,failed],|r|Ok(MediaRequest{thread:r.get(0)?,external:r.get(1)?,file:r.get(2)?,attempts:r.get(3)?,error:r.get(4)?}))?.collect::<rusqlite::Result<_>>()?;
        tx.commit()?;
        Ok(list)
    }
    pub fn record_media_failure(
        &self,
        who: &str,
        name: &str,
        r: &MediaRequest,
        error: &str,
    ) -> Result<()> {
        self.write(|tx|{self.authorized_source(tx,name,who)?;
            if tx.execute("UPDATE sync_media_fetch SET attempts=attempts+1,error=? WHERE source=? AND thread=? AND external=? AND file=? AND attempts=?",params![error.chars().take(300).collect::<String>(),name,r.thread,r.external,r.file,r.attempts])?!=0{
                if r.attempts>=2 && let Some(c)=mapping::find_on(tx,name,&r.thread,&r.external)?{
                    let m=Board::get_on(tx,c.message)?;Self::changed(tx,"attachment.unavailable",who,&m,json!({}),false)?;
                }else{Self::event_tx(tx,"source.file_deferred",who,None,json!({"source":name,"file":r.file}))?;}}Ok(())})
    }
    /// Copy bytes into the original imported message and record its external file ID in the same write.
    pub fn keep_source_file(
        &self,
        actor: &str,
        name: &str,
        thread: &str,
        message: &str,
        file: InboundFile<'_>,
    ) -> Result<Attachment> {
        let InboundFile {
            external,
            name: filename,
            mime,
            bytes,
        } = file;
        ident(external)?;
        files::valid_file(filename, mime)?;
        ensure!(
            bytes.len() as i64 <= files::MAX_DIRECT_BYTES,
            "file exceeds 100 MiB"
        );
        self.write(|tx|{
            let s=self.authorized_source(tx,name,actor)?;
            let c=mapping::find_on(tx,name,thread,message)?.context("message not linked at this source")?;
            ensure!(c.origin,"only imported messages may acquire external files");let old=Self::get_on(tx,c.message)?;
            ensure!(!old.deleted,"cannot attach a file to a deleted message");
            if let Some(id)=tx.query_row("SELECT asset FROM sync_file_copies WHERE source=? AND thread=? AND message=? AND external=? AND direction='in'",
                params![name,thread,c.message,external],|r|r.get(0)).optional()?{return Self::attachment_on(tx,id);}
            ensure!(old.attachments.len()<crate::MAX_ATTACHMENTS,"message already has the maximum number of files");
            let id=files::add(tx,&old.actor,&s.channel,filename,mime,bytes.len() as i64,bytes)?;
            tx.execute("INSERT INTO message_files VALUES(?,?,?)",params![c.message,old.attachments.len() as i64,id])?;
            tx.execute("INSERT INTO sync_file_copies VALUES(?,?,?,?,?,'in')",params![name,thread,external,c.message,id])?;
            tx.execute("DELETE FROM sync_media_fetch WHERE source=? AND thread=? AND external=? AND file=?",params![name,thread,message,external])?;
            Self::changed(tx,"attachment.added",&old.actor,&old,json!({"id":id}),false)?;Self::attachment_on(tx,id)
        })
    }
    pub fn source_files(&self, actor: &str, name: &str, message: i64) -> Result<Vec<SourceFile>> {
        let tx = self.conn.unchecked_transaction()?;
        self.authorized_source(&tx, name, actor)?;
        let files=tx.prepare("SELECT external,asset,direction,thread FROM sync_file_copies WHERE source=? AND message=? ORDER BY asset")?
            .query_map(params![name,message],|r|Ok(SourceFile{external:r.get(0)?,message,asset:r.get(1)?,thread:r.get(3)?,direction:if r.get::<_,String>(2)?=="in"{Direction::In}else{Direction::Out}}))?.collect::<rusqlite::Result<_>>()?;
        tx.commit()?;
        Ok(files)
    }
    pub fn stage_source_file(
        &self,
        actor: &str,
        name: &str,
        thread: &str,
        message: i64,
        asset: i64,
    ) -> Result<Job> {
        self.write(|tx| {
            let s = self.authorized_source(tx, name, actor)?;
            ensure!(
                s.outbound && s.provider == Provider::Slack,
                "file publishing requires an explicitly enabled Slack source"
            );
            let m = Self::get_on(tx, message)?;
            ensure!(
                !m.deleted
                    && !m.no_sync
                    && m.channel == s.channel
                    && m.attachments.iter().any(|a| a.id == asset),
                "expected a live shareable message carrying that file in the mapped channel"
            );
            let root = mapping::root_on(tx, name, thread)?.context("thread is not linked")?;
            ensure!(root == m.re.unwrap_or(m.id), "incorrect target thread");
            let root = Self::get_on(tx, root)?;
            ensure!(
                !root.no_sync && !root.deleted,
                "cannot share a file from this thread"
            );
            let before = tx.total_changes();
            let id = queue_asset(tx, &s, thread, &m, asset, false)?;
            if tx.total_changes() != before {
                Self::event_tx(
                    tx,
                    "source.file_requested",
                    actor,
                    Some(message),
                    json!({"source":name,"item":id,"asset":asset}),
                )?;
            }
            queue::get(tx, name, id)
        })
    }
}
