use super::*;
use crate::{Attachment, files};

#[derive(Debug, Serialize)]
pub struct SourceFile {
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

impl Board {
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
            Self::changed(tx,"attachment.added",&old.actor,&old,json!({"id":id}),false)?;Self::attachment_on(tx,id)
        })
    }
    pub fn source_files(&self, actor: &str, name: &str, message: i64) -> Result<Vec<SourceFile>> {
        let tx = self.conn.unchecked_transaction()?;
        self.authorized_source(&tx, name, actor)?;
        let files=tx.prepare("SELECT external,asset,direction FROM sync_file_copies WHERE source=? AND message=? ORDER BY asset")?
            .query_map(params![name,message],|r|Ok(SourceFile{external:r.get(0)?,message,asset:r.get(1)?,direction:if r.get::<_,String>(2)?=="in"{Direction::In}else{Direction::Out}}))?.collect::<rusqlite::Result<_>>()?;
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
        self.write(|tx|{
            let s=self.authorized_source(tx,name,actor)?;
            ensure!(s.outbound&&s.provider==Provider::Slack,"file publishing requires an explicitly enabled Slack source");
            let m=Self::get_on(tx,message)?;ensure!(!m.deleted&&m.channel==s.channel&&m.attachments.iter().any(|a|a.id==asset),"expected a live message carrying that file in the mapped channel");
            let root=mapping::root_on(tx,name,thread)?.context("thread is not linked")?;ensure!(root==m.re.unwrap_or(m.id),"incorrect target thread");
            let key=format!("file:{message}:{asset}");
            let d=Delivery{event:"file.share".into(),message,cursor:m.version,parent:None,text:String::new(),emoji:None,asset:Some(asset)};
            let n=tx.execute("INSERT OR IGNORE INTO sync_queue(source,direction,thread,event_key,payload,raw) VALUES(?,'out',?,?,?,'null')",params![name,thread,key,serde_json::to_string(&d)?])?;
            let id=tx.query_row("SELECT id FROM sync_queue WHERE source=? AND direction='out' AND thread=? AND event_key=?",params![name,thread,key],|r|r.get(0))?;
            if n!=0{Self::event_tx(tx,"source.file_requested",actor,Some(message),json!({"source":name,"item":id,"asset":asset}))?;}
            queue::get(tx,name,id)
        })
    }
}
