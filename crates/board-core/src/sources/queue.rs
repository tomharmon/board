use super::*;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    In,
    Out,
}
impl Direction {
    fn sql(self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Out => "out",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub key: String,
    pub import: Import,
    #[serde(default)]
    pub raw: Value,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Delivery {
    pub event: String,
    pub message: i64,
    pub cursor: i64,
    pub text: String,
    pub parent: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: i64,
    pub source: String,
    pub direction: Direction,
    pub thread: String,
    pub key: String,
    pub state: String,
    pub attempts: i64,
    pub payload: Value,
    pub raw: Value,
    pub result: Option<i64>,
    pub error: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct Lease {
    pub job: Job,
    pub receipt: String,
    pub deadline: i64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DeliveryResult {
    pub external: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
}
pub struct FailureReport<'a> {
    pub reason: &'a str,
    pub uncertain: bool,
    pub retry_after: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeliveryNotice {
    pub source: String,
    pub id: i64,
    pub event: String,
    pub state: String,
    pub attempts: i64,
    pub error: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    pub stream: String,
    pub expected: Option<String>,
    pub next: String,
}
#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct DrainReport {
    pub applied: usize,
    pub failed: usize,
    pub skipped: usize,
}
impl DrainReport {
    pub fn processed(&self) -> usize {
        self.applied + self.failed + self.skipped
    }
}
const COLS: &str = "id,source,direction,thread,event_key,state,attempts,payload,raw,result,error";
fn item(r: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    let j = |n| -> rusqlite::Result<Value> {
        serde_json::from_str(&r.get::<_, String>(n)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(n, rusqlite::types::Type::Text, Box::new(e))
        })
    };
    Ok(Job {
        id: r.get(0)?,
        source: r.get(1)?,
        direction: if r.get::<_, String>(2)? == "in" {
            Direction::In
        } else {
            Direction::Out
        },
        thread: r.get(3)?,
        key: r.get(4)?,
        state: r.get(5)?,
        attempts: r.get(6)?,
        payload: j(7)?,
        raw: j(8)?,
        result: r.get(9)?,
        error: r.get(10)?,
    })
}
pub(super) fn get(c: &Connection, s: &str, id: i64) -> Result<Job> {
    c.query_row(
        &format!("SELECT {COLS} FROM sync_queue WHERE source=? AND id=?"),
        params![s, id],
        item,
    )
    .optional()?
    .with_context(|| Kind::Missing.error("inbox_not_found", "unknown delivery"))
}
fn time() -> i64 {
    chrono::Utc::now().timestamp()
}
fn expire(tx: &Transaction<'_>, name: &str, now: i64) -> Result<()> {
    tx.execute("UPDATE sync_queue SET state=CASE WHEN state='sending' THEN 'uncertain' WHEN attempts>=10 THEN 'failed' ELSE 'pending' END, error=CASE WHEN state='sending' THEN 'delivery outcome unknown after interruption' WHEN attempts>=10 THEN 'receipt expired repeatedly' ELSE error END,receipt=NULL,deadline=NULL WHERE source=? AND state IN ('sending','leased') AND deadline<=?",params![name,now])?;
    Ok(())
}
fn push(tx: &Transaction<'_>, s: &Source, e: &Envelope) -> Result<(i64, bool)> {
    ensure!(
        !e.key.is_empty()
            && e.key.trim() == e.key
            && !e.key.chars().any(char::is_control)
            && e.key.encode_utf16().count() <= 300,
        "a source delivery key must have 1..300 UTF-16 code units"
    );
    mapping::validate(&e.import)?;
    let data = serde_json::to_string(&e.import)?;
    let raw = serde_json::to_string(&e.raw)?;
    ensure!(raw.len() <= 512 * 1024, "raw source event exceeds 512 KiB");
    let fresh=tx.execute("INSERT OR IGNORE INTO sync_queue(source,direction,thread,event_key,payload,raw) VALUES(?,'in',?,?,?,?)",params![s.name,e.import.thread,e.key,data,raw])?!=0;
    let (id,stored):(i64,String)=tx.query_row("SELECT id,payload FROM sync_queue WHERE source=? AND direction='in' AND thread=? AND event_key=?",params![s.name,e.import.thread,e.key],|r|Ok((r.get(0)?,r.get(1)?)))?;
    ensure!(
        stored == data,
        Kind::Conflict.error("source_replay", "inbox key used for a different event")
    );
    if fresh {
        Board::event_tx(
            tx,
            "source.received",
            &s.owner,
            None,
            json!({"source":s.name,"channel":s.channel,"item":id}),
        )?;
    }
    Ok((id, fresh))
}
fn check_receipt(c: &Connection, s: &str, id: i64, token: &str) -> Result<Job> {
    let j = get(c, s, id)?;
    let valid:bool=c.query_row("SELECT state IN ('leased','sending') AND coalesce(receipt=? AND deadline>?,0) FROM sync_queue WHERE id=?",params![token,time(),id],|r|r.get(0))?;
    ensure!(
        valid,
        Kind::Conflict.error(
            "stale_receipt",
            "receipt is expired or does not own this delivery"
        )
    );
    Ok(j)
}
// Never send an obsolete post just because its event sat in an offline outbox.
pub(super) fn abandon_skipped_starts(tx: &Transaction<'_>, name: &str) -> Result<()> {
    let lanes=tx.prepare("SELECT q.thread FROM sync_starts s JOIN sync_queue q ON q.id=s.item JOIN sync_threads t ON t.source=q.source AND t.thread=q.thread AND t.root=s.root WHERE s.source=? AND q.state='skipped' AND NOT EXISTS(SELECT 1 FROM sync_copies c WHERE c.source=t.source AND c.thread=t.thread)")?
        .query_map([name],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for lane in lanes {
        tx.execute("UPDATE sync_queue SET state='skipped',error='thread not started',receipt=NULL,deadline=NULL WHERE source=? AND direction='out' AND thread=? AND state IN ('pending','leased')",params![name,lane])?;
        tx.execute(
            "DELETE FROM sync_threads WHERE source=? AND thread=?",
            params![name, lane],
        )?;
        tx.execute("DELETE FROM sync_starts WHERE source=? AND item IN (SELECT id FROM sync_queue WHERE source=? AND thread=? AND direction='out' AND state='skipped')",params![name,name,lane])?;
    }
    Ok(())
}
fn refresh_outbound(tx: &Transaction<'_>, source: &Source, id: i64) -> Result<bool> {
    let job = get(tx, &source.name, id)?;
    if job.payload["event"] == "source.response" {
        if !tx.query_row("SELECT EXISTS(SELECT 1 FROM sync_threads t JOIN messages m ON m.id=t.root LEFT JOIN private_messages p ON p.id=m.id WHERE t.source=? AND t.thread=? AND (p.id IS NOT NULL OR m.deleted))",params![source.name,job.thread],|r|r.get::<_,bool>(0))?{return Ok(true);}
        tx.execute(
            "UPDATE sync_queue SET state='skipped',error='thread no longer shareable',receipt=NULL,deadline=NULL WHERE id=?",
            [id],
        )?;
        return Ok(false);
    }
    let mut d: Delivery = serde_json::from_value(job.payload)?;
    let m = Board::get_on(tx, d.message)?;
    let predates_join: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sync_explicit_joins WHERE source=? AND thread=? AND joined_at>=?)",
        params![source.name, job.thread, m.id], |r| r.get(0))?;
    let private: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM private_messages WHERE id IN (?,?))",
        params![m.id, m.re],
        |r| r.get(0),
    )?;
    let root = m.re.unwrap_or(m.id);
    let root_deleted: bool =
        tx.query_row("SELECT deleted FROM messages WHERE id=?", [root], |r| {
            r.get(0)
        })?;
    let origin: Option<bool> = tx
        .query_row(
            "SELECT origin FROM sync_copies WHERE source=? AND thread=? AND message=?",
            params![source.name, job.thread, m.id],
            |r| r.get(0),
        )
        .optional()?;
    let copy = origin.is_some();
    let last: Option<String> = tx
        .query_row(
            "SELECT text FROM sync_rendered WHERE source=? AND thread=? AND message=?",
            params![source.name, job.thread, m.id],
            |r| r.get(0),
        )
        .optional()?;
    let mut current = match d.event.as_str() {
        "thread.start" | "message.posted" => !m.deleted && !root_deleted && !copy,
        "message.edited" => !m.deleted && origin == Some(false),
        "message.deleted" => origin == Some(false),
        "file.share" => !m.deleted && !root_deleted,
        "file.delete" => true,
        _ => !m.deleted && copy,
    };
    if predates_join {
        current = false;
    }
    if current
        && !private
        && matches!(
            d.event.as_str(),
            "thread.start" | "message.posted" | "message.edited" | "file.note"
        )
    {
        match render::outbound(tx, source, &job.thread, &m) {
            Ok(text) => {
                if d.event == "message.edited" && last.as_deref() == Some(&text) {
                    current = false;
                }
                d.text = text;
            }
            Err(e) => {
                tx.execute(
                    "UPDATE sync_queue SET state='failed',error=?,receipt=NULL,deadline=NULL WHERE id=?",
                    params![e.to_string(), id],
                )?;
                Board::event_tx(
                    tx,
                    "source.delivery_failed",
                    &source.owner,
                    Some(m.id),
                    json!({"source":source.name,"item":id,"state":"failed"}),
                )?;
                return Ok(false);
            }
        }
    }
    if private || !current {
        tx.execute(
            "UPDATE sync_queue SET state='skipped',error='superseded before delivery',receipt=NULL,deadline=NULL WHERE id=?",
            [id],
        )?;
        if d.event == "thread.start" {
            abandon_skipped_starts(tx, &source.name)?;
        }
        Board::event_tx(
            tx,
            "source.skipped",
            &source.owner,
            Some(m.id),
            json!({"source":source.name,"item":id}),
        )?;
        return Ok(false);
    }
    if matches!(
        d.event.as_str(),
        "thread.start" | "message.posted" | "message.edited" | "file.note"
    ) {
        tx.execute(
            "UPDATE sync_queue SET payload=? WHERE id=?",
            params![serde_json::to_string(&d)?, id],
        )?;
    }
    Ok(true)
}
fn clear_to_import(tx: &Transaction<'_>, source: &str, thread: &str) -> Result<()> {
    let waiting:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sync_queue WHERE source=? AND direction='out' AND state IN ('sending','uncertain')
         AND (thread=? OR json_extract(payload,'$.event')='thread.start'))",params![source,thread],|r|r.get(0))?;
    ensure!(
        !waiting,
        Kind::Conflict.error(
            "awaiting_receipt",
            "reconcile the outstanding remote write before importing its possible echo"
        )
    );
    Ok(())
}
fn complete(
    tx: &Transaction<'_>,
    s: &Source,
    j: &Job,
    remote: Option<&DeliveryResult>,
) -> Result<Message> {
    let m = match j.direction {
        Direction::In => {
            ensure!(remote.is_none(), "no remote receipt for an inbound event");
            clear_to_import(tx, &s.name, &j.thread)?;
            let input: Import = serde_json::from_value(j.payload.clone())?;
            ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM sync_responses WHERE source=? AND thread=? AND external=?)",params![s.name,input.thread,input.external],|r|r.get::<_,bool>(0))?,Kind::Invalid.error("response_echo","already-confirmed policy response"));
            tx.execute(
                "INSERT INTO sync_import_context VALUES(1,?,?,?)",
                params![s.name, input.thread, input.external],
            )?;
            let mut m = mapping::apply(tx, s, &input)?;
            let current_slack_origin=s.provider==Provider::Slack&&!m.deleted&&m.origin.as_ref().is_some_and(|c|c.source==s.name&&c.thread==input.thread&&c.external==input.external)
                && tx.query_row("SELECT ?4 IS NULL OR modified IS NULL OR modified<=?4 FROM sync_copies WHERE source=?1 AND thread=?2 AND external=?3",params![s.name,input.thread,input.external,input.updated_at.map(|t|t.timestamp_millis())],|r|r.get::<_,bool>(0))?;
            if current_slack_origin
                && !input.links.is_empty()
                && crate::links::enrich(tx, m.id, &input.links)?
            {
                m = Board::changed(tx, "message.links", &m.actor, &m, json!({}), false)?;
            }
            if current_slack_origin && s.settings.mirror_files {
                let files = j
                    .raw
                    .get("files")
                    .or_else(|| j.raw.pointer("/event/message/files"))
                    .or_else(|| j.raw.pointer("/event/files"));
                for (position, file) in files
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .take(100)
                    .enumerate()
                {
                    if let Some(id) = file["id"].as_str().filter(|x| {
                        !x.is_empty()
                            && x.len() <= 128
                            && x.bytes().all(|b| b.is_ascii_alphanumeric())
                    }) {
                        let size = file["size"].as_i64();
                        let reason = if position >= crate::MAX_ATTACHMENTS {
                            Some("more than ten attachments")
                        } else if size == Some(0) {
                            Some("empty file")
                        } else if size.is_some_and(|v| v > 25 * 1024 * 1024) {
                            Some("over the 25 MiB automatic transfer limit")
                        } else {
                            None
                        };
                        tx.execute(
                            "INSERT OR IGNORE INTO sync_file_metadata VALUES(?,?,?,?,?,?,?)",
                            params![
                                s.name,
                                input.thread,
                                input.external,
                                id,
                                m.id,
                                file["name"]
                                    .as_str()
                                    .unwrap_or(id)
                                    .chars()
                                    .take(255)
                                    .collect::<String>(),
                                size
                            ],
                        )?;
                        let new=tx.execute("INSERT OR IGNORE INTO sync_media_fetch(source,thread,external,file,attempts,error) VALUES(?,?,?,?,?,?)",params![s.name,input.thread,input.external,id,if reason.is_some(){3}else{0},reason])?;
                        if new != 0 && reason.is_some() {
                            Board::changed(
                                tx,
                                "attachment.unavailable",
                                &m.actor,
                                &m,
                                json!({}),
                                false,
                            )?;
                        }
                    }
                }
            }
            tx.execute("DELETE FROM sync_import_context WHERE singleton=1", [])?;
            m
        }
        Direction::Out => {
            let d: Delivery = serde_json::from_value(j.payload.clone())?;
            let m = Board::get_on(tx, d.message)?;
            let r = remote.context("an outbound delivery requires its external message id")?;
            let target = if d.event == "thread.start" {
                let target = r
                    .thread
                    .as_deref()
                    .context("starting a thread requires the confirmed external thread id")?;
                ident(target)?;
                ensure!(target != j.thread, "provider must supply a final thread id");
                ensure!(
                    mapping::root_on(tx, &s.name, target)?.is_none(),
                    Kind::Conflict.error("source_thread", "remote thread is already bound")
                );
                tx.execute(
                    "INSERT INTO sync_threads VALUES(?,?,?)",
                    params![s.name, target, m.id],
                )?;
                tx.execute("UPDATE sync_queue SET thread=? WHERE source=? AND direction='out' AND thread=?",params![target,s.name,j.thread])?;
                tx.execute(
                    "DELETE FROM sync_threads WHERE source=? AND thread=?",
                    params![s.name, j.thread],
                )?;
                target
            } else {
                ensure!(
                    r.thread.as_deref().is_none_or(|t| t == j.thread),
                    "unexpected target change"
                );
                &j.thread
            };
            if let Some(asset) = d.asset {
                ident(&r.external)?;
                if d.event == "file.delete" {
                    ensure!(tx.execute("DELETE FROM sync_file_copies WHERE source=? AND thread=? AND message=? AND asset=? AND external=? AND direction='out'",params![s.name,target,m.id,asset,r.external])?==1,"unknown published file");
                } else {
                    tx.execute(
                        "INSERT INTO sync_file_copies VALUES(?,?,?,?,?,'out')",
                        params![s.name, target, r.external, m.id, asset],
                    )?;
                    if m.deleted {
                        media::queue_asset(tx, s, target, &m, asset, true)?;
                    }
                }
            } else {
                mapping::bind(tx, s, target, &r.external, &m, false, r.url.as_deref())?;
                if matches!(
                    d.event.as_str(),
                    "thread.start" | "message.posted" | "message.edited" | "file.note"
                ) {
                    tx.execute("INSERT INTO sync_rendered VALUES(?,?,?,?) ON CONFLICT(source,thread,message) DO UPDATE SET text=excluded.text",params![s.name,target,m.id,d.text])?;
                    if d.event != "file.note" && s.settings.mirror_files && !m.deleted && !m.no_sync
                    {
                        for file in &m.attachments {
                            media::queue_asset(tx, s, target, &m, file.id, false)?;
                        }
                    }
                }
            }
            m
        }
    };
    tx.execute("UPDATE sync_queue SET state='done',result=?,receipt=NULL,deadline=NULL,error=NULL WHERE id=?",params![m.id,j.id])?;
    Board::event_tx(
        tx,
        "source.receipt",
        &s.owner,
        Some(m.id),
        json!({"source":s.name,"item":j.id,"direction":j.direction}),
    )?;
    Ok(m)
}
/// Keep a deliberate skip and its optional remote response, but none of the rejected write.
fn try_import(tx: &Transaction<'_>, s: &Source, j: &Job) -> Result<Result<Message>> {
    tx.execute_batch("SAVEPOINT inbound_policy")?;
    match complete(tx, s, j, None) {
        Ok(m) => {
            tx.execute_batch("RELEASE inbound_policy")?;
            Ok(Ok(m))
        }
        Err(e)
            if matches!(
                crate::errors::classify(&e).1,
                "dropped" | "response_echo" | "ignored_event"
            ) =>
        {
            tx.execute_batch("ROLLBACK TO inbound_policy; RELEASE inbound_policy")?;
            tx.execute("UPDATE sync_queue SET state='skipped',receipt=NULL,deadline=NULL,error=? WHERE id=?",params![e.to_string(),j.id])?;
            if s.outbound
                && j.payload["no_sync"] != true
                && let Some(response) = e
                    .downcast_ref::<crate::errors::Rejection>()
                    .and_then(|r| r.response())
            {
                tx.execute("INSERT OR IGNORE INTO sync_queue(source,direction,thread,event_key,payload,raw) VALUES(?,'out',?,?,?,'null')",
                    params![s.name,j.thread,format!("response:{}",j.id),json!({"event":"source.response","text":response}).to_string()])?;
            }
            Board::event_tx(
                tx,
                "source.skipped",
                &s.owner,
                None,
                json!({"source":s.name,"item":j.id,"reason":crate::errors::classify(&e).1}),
            )?;
            Ok(Err(e))
        }
        Err(e) => Err(e),
    }
}
fn response_receipt(tx: &Transaction<'_>, s: &Source, j: &Job, r: &DeliveryResult) -> Result<()> {
    ident(&r.external)?;
    ensure!(
        r.thread.as_deref().is_none_or(|t| t == j.thread),
        "unexpected response thread"
    );
    tx.execute(
        "INSERT INTO sync_responses VALUES(?,?,?,?)",
        params![s.name, j.thread, r.external, j.id],
    )?;
    tx.execute(
        "UPDATE sync_queue SET state='done',receipt=NULL,deadline=NULL,error=NULL WHERE id=?",
        [j.id],
    )?;
    Board::event_tx(
        tx,
        "source.receipt",
        &s.owner,
        None,
        json!({"source":s.name,"item":j.id,"direction":"out"}),
    )?;
    Ok(())
}

impl Board {
    /// Latest 100 outbound receipts and outstanding writes for one exact message. Do not expose raw inbox data.
    pub fn message_deliveries(&self, who: &str, id: i64) -> Result<Vec<DeliveryNotice>> {
        let tx = self.conn.unchecked_transaction()?;
        let a = self.access_on(&tx)?;
        a.actor(who)?;
        a.message(&tx, id, crate::access::Action::Read)?;
        let m = Self::get_on(&tx, id)?;
        if a.integration_narrowed() || !a.allows(crate::access::Action::Sync, &m.channel) {
            return Ok(Vec::new());
        }
        let list=tx.prepare("SELECT q.source,q.id,json_extract(q.payload,'$.event'),q.state,q.attempts,q.error FROM sync_queue q JOIN sync_sources s ON s.name=q.source
            WHERE s.channel=?1 AND (s.owner=?2 OR instr(s.owner,?2||'/')=1) AND q.direction='out' AND json_extract(q.payload,'$.message')=?3 ORDER BY q.id DESC LIMIT 100")?
            .query_map(params![m.channel,who,id],|r|Ok(DeliveryNotice{source:r.get(0)?,id:r.get(1)?,event:r.get(2)?,state:r.get(3)?,attempts:r.get(4)?,error:r.get(5)?}))?.collect::<rusqlite::Result<_>>()?;
        tx.commit()?;
        Ok(list)
    }
    /// A provider definitely refused a file before receiving bytes. Preserve the text copy, render a local reference once.
    pub fn refused_file(&self, who: &str, name: &str, id: i64, receipt: &str) -> Result<()> {
        self.write(|tx|{let s=self.authorized_source(tx,name,who)?;let job=check_receipt(tx,name,id,receipt)?;
            let d:Delivery=serde_json::from_value(job.payload)?;ensure!(job.direction==Direction::Out&&d.event=="file.share"&&job.state=="sending","expected a sending file");
            let asset=d.asset.context("missing file")?;let m=Board::get_on(tx,d.message)?;
            tx.execute("UPDATE sync_queue SET state='skipped',error='file rejected before transfer',receipt=NULL,deadline=NULL WHERE id=?",[id])?;
            if !m.deleted && tx.query_row("SELECT EXISTS(SELECT 1 FROM sync_copies WHERE source=? AND thread=? AND message=? AND origin=0)",params![name,job.thread,m.id],|r|r.get::<_,bool>(0))? {
                tx.execute("INSERT OR IGNORE INTO sync_file_notes VALUES(?,?,?,?)",params![name,job.thread,m.id,asset])?;
                let note=Delivery{event:"file.note".into(),asset:None,text:m.text.clone(),..d};
                tx.execute("INSERT OR IGNORE INTO sync_queue(source,direction,thread,event_key,payload,raw) VALUES(?,'out',?,?,?,'null')",params![name,job.thread,format!("note:{id}"),serde_json::to_string(&note)?])?;
            }
            Self::event_tx(tx,"source.file_deferred",who,Some(m.id),json!({"source":s.name,"item":id,"asset":asset}))?;Ok(())})
    }
    pub fn declined_assets(
        &self,
        who: &str,
        name: &str,
        thread: &str,
        id: i64,
    ) -> Result<Vec<i64>> {
        let tx = self.conn.unchecked_transaction()?;
        self.authorized_source(&tx, name, who)?;
        let rows = render::declined_files(&tx, name, thread, id)?;
        tx.commit()?;
        Ok(rows)
    }
    /// Intentionally start a remote conversation from a local root; a stopped/offline connector does nothing.
    pub fn start_source_thread(&self, who: &str, name: &str, root: i64) -> Result<Job> {
        self.write(|tx| self.start_source_thread_on(tx, who, name, root))
    }
    pub(super) fn start_source_thread_on(
        &self,
        tx: &Transaction<'_>,
        who: &str,
        name: &str,
        root: i64,
    ) -> Result<Job> {
        let s = self.authorized_source(tx, name, who)?;
        ensure!(s.outbound, "publishing must be explicitly enabled");
        ensure!(
            s.settings.github_items != GithubItems::Pulls,
            "a pull-request-only source cannot open an issue"
        );
        let m = Self::get_on(tx, root)?;
        ensure!(
            m.channel == s.channel && m.re.is_none() && !m.no_sync && !m.deleted,
            "expected a live shareable root in the mapped channel"
        );
        if let Some(id) = tx
            .query_row(
                "SELECT item FROM sync_starts WHERE source=? AND root=?",
                params![name, root],
                |r| r.get(0),
            )
            .optional()?
        {
            return get(tx, name, id);
        }
        ensure!(
            !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sync_threads WHERE source=? AND root=?)",
                params![name, root],
                |r| r.get::<_, bool>(0)
            )?,
            Kind::Conflict.error("already_linked", "thread already linked at this provider")
        );
        let lane = format!("pending:{}", Uuid::new_v4());
        tx.execute(
            "INSERT INTO sync_threads VALUES(?,?,?)",
            params![name, lane, root],
        )?;
        let d = Delivery {
            event: "thread.start".into(),
            message: root,
            cursor: root,
            text: m.text,
            parent: None,
            emoji: None,
            asset: None,
        };
        tx.execute("INSERT INTO sync_queue(source,direction,thread,event_key,payload,raw) VALUES(?,'out',?,?,?,'null')",params![name,lane,root.to_string(),serde_json::to_string(&d)?])?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO sync_starts VALUES(?,?,?)",
            params![name, root, id],
        )?;
        backfill(tx, &s, &lane, root)?;
        Self::event_tx(
            tx,
            "thread.sync_requested",
            who,
            Some(root),
            json!({"source":name,"item":id}),
        )?;
        get(tx, name, id)
    }
    pub fn backfill_source(&self, who: &str, name: &str, thread: &str) -> Result<usize> {
        self.write(|tx| {
            let s = self.authorized_source(tx, name, who)?;
            ensure!(s.outbound, "publishing must be explicitly enabled");
            let root = mapping::root_on(tx, name, thread)?.context("thread is not linked")?;
            let count = backfill(tx, &s, thread, root)?;
            if count > 0 {
                Self::event_tx(
                    tx,
                    "source.backfill_requested",
                    who,
                    Some(root),
                    json!({"source":name,"thread":thread,"queued":count}),
                )?;
            }
            Ok(count)
        })
    }
    /// Enqueue inputs and advance their remote page cursor together. Refetching a committed page is harmless.
    pub fn enqueue_imports(
        &self,
        who: &str,
        name: &str,
        events: &[Envelope],
        position: Option<&Checkpoint>,
    ) -> Result<Vec<i64>> {
        ensure!(events.len() <= 500, "up to 500 events per inbox batch");
        self.write(|tx|{let s=self.authorized_source(tx,name,who)?;
            let ids=events.iter().map(|e|push(tx,&s,e).map(|v|v.0)).collect::<Result<Vec<_>>>()?;
            if let Some(p)=position{
                ident(&p.stream)?; ident(&p.next)?;
                let old:Option<String>=tx.query_row("SELECT value FROM sync_checkpoints WHERE source=? AND stream=?",params![name,p.stream],|r|r.get(0)).optional()?;
                ensure!(old==p.expected||old.as_deref()==Some(&p.next),Kind::Conflict.error("checkpoint_conflict","another poll advanced this checkpoint"));
                tx.execute("INSERT INTO sync_checkpoints VALUES(?,?,?) ON CONFLICT(source,stream) DO UPDATE SET value=excluded.value",params![name,p.stream,p.next])?;
            }Ok(ids)})
    }
    /// Apply a normalized event synchronously, without a worker or network.
    pub fn import_source(&self, who: &str, name: &str, e: &Envelope) -> Result<Message> {
        self.write(|tx|{let s=self.authorized_source(tx,name,who)?;let (id,_)=push(tx,&s,e)?;let j=get(tx,name,id)?;
            if j.state=="done"{return Ok(Ok(self.access_on(tx)?.view(tx,Self::get_on(tx,j.result.context("delivery has no result")?)?)?));}
            if j.state=="skipped"{return Ok(Err(Kind::Invalid.error("dropped",j.error.unwrap_or_default()).into()));}
            ensure!(j.state=="pending"&&!tx.query_row("SELECT EXISTS(SELECT 1 FROM sync_queue WHERE source=? AND direction='in' AND thread=? AND id<? AND state NOT IN ('done','skipped'))",params![name,j.thread,id],|r|r.get::<_,bool>(0))?,Kind::Conflict.error("lane_busy","an earlier delivery or lease is outstanding"));
            Ok(match try_import(tx,&s,&j)?{Ok(m)=>Ok(self.access_on(tx)?.view(tx,m)?),Err(e)=>Err(e)})})?
    }
    pub fn inbox(
        &self,
        who: &str,
        name: &str,
        dir: Direction,
        after: i64,
        limit: usize,
    ) -> Result<Vec<Job>> {
        Ok(self.inbox_page(who, name, dir, after, limit)?.1)
    }
    /// Unlike the board event cursor, this continuation advances through this source queue's IDs.
    pub fn inbox_page(
        &self,
        who: &str,
        name: &str,
        dir: Direction,
        after: i64,
        limit: usize,
    ) -> Result<(i64, Vec<Job>)> {
        ensure!(
            after >= 0 && (1..=500).contains(&limit),
            "invalid cursor or limit"
        );
        let tx = self.conn.unchecked_transaction()?;
        self.authorized_source(&tx, name, who)?;
        let rows=tx.prepare(&format!("SELECT {COLS} FROM sync_queue WHERE source=? AND direction=? AND id>? ORDER BY id LIMIT ?"))?.query_map(params![name,dir.sql(),after,limit as i64],item)?.collect::<rusqlite::Result<Vec<_>>>()?;
        let cursor = match rows.last() {
            Some(last) => last.id,
            None => tx.query_row(
                "SELECT coalesce(max(id),0) FROM sync_queue WHERE source=? AND direction=?",
                params![name, dir.sql()],
                |r| r.get(0),
            )?,
        };
        tx.commit()?;
        Ok((cursor, rows))
    }
    pub fn queue_item(&self, who: &str, name: &str, id: i64) -> Result<Job> {
        let tx = self.conn.unchecked_transaction()?;
        self.authorized_source(&tx, name, who)?;
        let j = get(&tx, name, id)?;
        tx.commit()?;
        Ok(j)
    }
    pub fn lease_source(
        &self,
        who: &str,
        name: &str,
        dir: Direction,
        seconds: i64,
    ) -> Result<Option<Lease>> {
        self.lease_source_inner(who, name, dir, seconds, false)
    }
    fn lease_source_inner(
        &self,
        who: &str,
        name: &str,
        dir: Direction,
        seconds: i64,
        arm_outbound: bool,
    ) -> Result<Option<Lease>> {
        ensure!(
            (1..=3600).contains(&seconds),
            "lease must be 1..3600 seconds"
        );
        self.write(|tx|{let s=self.authorized_source(tx,name,who)?;let now=time();expire(tx,name,now)?;
            if dir==Direction::Out && !s.outbound {return Ok(None);}
            loop{let id:Option<i64>=tx.query_row("SELECT q.id FROM sync_queue q WHERE source=? AND direction=? AND state='pending' AND available<=?
                AND (q.direction!='in' OR NOT EXISTS(SELECT 1 FROM sync_queue o WHERE o.source=q.source AND o.direction='out' AND o.state IN ('sending','uncertain') AND (o.thread=q.thread OR json_extract(o.payload,'$.event')='thread.start')))
                AND NOT EXISTS(SELECT 1 FROM sync_queue h WHERE h.source=q.source AND h.direction=q.direction AND h.thread=q.thread AND h.id<q.id AND h.state NOT IN ('done','skipped') AND (h.state!='failed' OR h.direction='in' OR json_extract(h.payload,'$.event')='thread.start')) ORDER BY q.id LIMIT 1",
                params![name,dir.sql(),now],|r|r.get(0)).optional()?;
            let Some(id)=id else{return Ok(None)};
            if dir==Direction::Out && !refresh_outbound(tx,&s,id)?{continue;}
            let receipt=Uuid::new_v4().to_string();let deadline=now+seconds;
            let state=if arm_outbound && dir==Direction::Out { "sending" } else { "leased" };
            tx.execute("UPDATE sync_queue SET state=?,attempts=attempts+1,receipt=?,deadline=? WHERE id=?",params![state,receipt,deadline,id])?;
            return Ok(Some(Lease{job:get(tx,name,id)?,receipt,deadline}));}})
    }
    /// Commit the intent before an external write. A crash after this point never silently repeats a remote POST.
    pub fn start_sending(&self, who: &str, name: &str, id: i64, receipt: &str) -> Result<()> {
        let started = self.write(|tx| {
            let s = self.authorized_source(tx, name, who)?;
            let j = check_receipt(tx, name, id, receipt)?;
            ensure!(
                s.outbound && j.direction == Direction::Out && j.state == "leased",
                Kind::Conflict.error(
                    "not_sendable",
                    "publishing is disabled or the delivery is not newly leased"
                )
            );
            if !refresh_outbound(tx,&s,id)? {
                return Ok(false);
            }
            if get(tx,name,id)?.payload != j.payload {
                // The holder prepared a different body. No remote mutation has been attempted.
                tx.execute("UPDATE sync_queue SET state='pending',receipt=NULL,deadline=NULL,attempts=attempts-1 WHERE id=?", [id])?;
                return Ok(false);
            }
            tx.execute("UPDATE sync_queue SET state='sending' WHERE id=?", [id])?;
            Ok(true)
        })?;
        ensure!(
            started,
            Kind::Conflict.error(
                "delivery_changed",
                "delivery changed before send; no remote write was armed"
            )
        );
        Ok(())
    }
    /// Hand an external consumer a lease. Returning an outbound receipt arms crash reconciliation.
    pub fn take_source_delivery(
        &self,
        who: &str,
        name: &str,
        dir: Direction,
        seconds: i64,
    ) -> Result<Option<Lease>> {
        self.lease_source_inner(who, name, dir, seconds, true)
    }
    pub fn finish_source(
        &self,
        who: &str,
        name: &str,
        id: i64,
        receipt: &str,
        result: Option<&DeliveryResult>,
    ) -> Result<Message> {
        self.ack_source(who, name, id, receipt, result)?
            .with_context(|| {
                Kind::Invalid.error("no_message", "delivery produced no Board message")
            })
    }
    pub fn ack_source(
        &self,
        who: &str,
        name: &str,
        id: i64,
        receipt: &str,
        result: Option<&DeliveryResult>,
    ) -> Result<Option<Message>> {
        self.write(|tx| {
            let s = self.authorized_source(tx, name, who)?;
            let j = check_receipt(tx, name, id, receipt)?;
            ensure!(
                j.direction == Direction::In || j.state == "sending",
                Kind::Conflict.error("not_sending", "record the external send intent first")
            );
            let m = if j.direction == Direction::In {
                ensure!(result.is_none(), "unexpected remote receipt");
                try_import(tx, &s, &j)?.ok()
            } else if j.payload["event"] == "source.response" {
                response_receipt(tx, &s, &j, result.context("expected remote response id")?)?;
                None
            } else {
                Some(complete(tx, &s, &j, result)?)
            };
            m.map(|m| self.access_on(tx)?.view(tx, m)).transpose()
        })
    }
    pub fn fail_source(
        &self,
        who: &str,
        name: &str,
        id: i64,
        receipt: &str,
        failure: FailureReport<'_>,
    ) -> Result<Job> {
        let FailureReport {
            reason: error,
            uncertain,
            retry_after: after,
        } = failure;
        ensure!(
            !error.trim().is_empty() && error.len() <= 2000,
            "error must be 1..2000 bytes"
        );
        self.write(|tx|{self.authorized_source(tx,name,who)?;let j=check_receipt(tx,name,id,receipt)?;
            ensure!(!uncertain||j.direction==Direction::Out,"only external writes have uncertain outcomes");
            let max=if j.direction==Direction::In{10}else{5};
            let state=if uncertain{"uncertain"}else if j.attempts>=max{"failed"}else{"pending"};
            let backoff=after.unwrap_or(1<<j.attempts.min(8)).clamp(1,86400);
            tx.execute("UPDATE sync_queue SET state=?,error=?,available=?,receipt=NULL,deadline=NULL WHERE id=?",params![state,error,time()+backoff,id])?;
            Board::event_tx(tx,"source.delivery_failed",who,None,json!({"source":name,"channel":src_on(tx,name)?.channel,"item":id,"state":state}))?;
            get(tx,name,id)})
    }
    pub fn resolve_source(
        &self,
        who: &str,
        name: &str,
        id: i64,
        action: &str,
        confirmed: Option<&DeliveryResult>,
    ) -> Result<Job> {
        self.write(|tx|{let s=self.authorized_source(tx,name,who)?;expire(tx,name,time())?;let j=get(tx,name,id)?;
            ensure!(matches!(j.state.as_str(),"failed"|"uncertain"),Kind::Conflict.error("not_failed","only failed or uncertain deliveries can be reconciled"));
            match action{
                "delivered"=>{ensure!(j.direction==Direction::Out,"inbound events cannot have remote receipts");if j.payload["event"]=="source.response"{response_receipt(tx,&s,&j,confirmed.context("expected response id")?)?;}else{complete(tx,&s,&j,confirmed)?;}}
                "retry"|"skip"=>{ensure!(confirmed.is_none(),"unexpected receipt");ensure!(action=="skip"||j.direction==Direction::In||s.outbound,"publishing is disabled");
                    tx.execute("UPDATE sync_queue SET state=?,attempts=0,available=0,error=NULL WHERE id=?",params![if action=="retry"{"pending"}else{"skipped"},id])?;
                    if action=="skip"&&j.payload["event"]=="thread.start"{abandon_skipped_starts(tx,name)?;}
                    Board::event_tx(tx,"source.reconciled",who,None,json!({"source":name,"channel":s.channel,"item":id,"action":action}))?;}
                _=>anyhow::bail!("expected delivered, retry or skip"),
            }get(tx,name,id)})
    }
    /// Drain stored input only. Never opens a socket.
    pub fn drain_source(&self, who: &str, name: &str, limit: usize) -> Result<(usize, usize)> {
        let report = self.drain_source_report(who, name, limit)?;
        Ok((report.applied, report.failed))
    }
    pub fn drain_source_report(&self, who: &str, name: &str, limit: usize) -> Result<DrainReport> {
        ensure!((1..=10000).contains(&limit), "limit must be 1..10000");
        let mut report = DrainReport::default();
        for _ in 0..limit {
            let Some(l) = self.lease_source(who, name, Direction::In, 60)? else {
                break;
            };
            match self.ack_source(who, name, l.job.id, &l.receipt, None) {
                Ok(Some(_)) => report.applied += 1,
                Ok(None) => report.skipped += 1,
                Err(e) => {
                    let msg = e.to_string().chars().take(450).collect::<String>();
                    self.fail_source(
                        who,
                        name,
                        l.job.id,
                        &l.receipt,
                        FailureReport {
                            reason: &msg,
                            uncertain: false,
                            retry_after: None,
                        },
                    )?;
                    report.failed += 1;
                }
            }
        }
        Ok(report)
    }
}

pub(super) fn backfill(tx: &Transaction<'_>, s: &Source, thread: &str, root: i64) -> Result<usize> {
    Ok(tx.execute("INSERT INTO sync_queue(source,direction,thread,event_key,payload,raw)
      SELECT ?1,'out',?2,CAST(m.id AS TEXT),json_object('event','message.posted','message',m.id,'cursor',m.id,'text',m.text,'parent',m.reply_to),'null'
      FROM messages m WHERE m.re=?3 AND m.deleted=0 AND EXISTS(SELECT 1 FROM messages WHERE id=?3 AND deleted=0)
      AND NOT EXISTS(SELECT 1 FROM sync_explicit_joins j WHERE j.source=?1 AND j.thread=?2 AND m.id<=j.joined_at)
      AND NOT EXISTS(SELECT 1 FROM private_messages WHERE id IN (m.id,m.re))
      AND NOT EXISTS (SELECT 1 FROM sync_copies sc WHERE sc.message=m.id AND sc.origin AND sc.source=?1) AND NOT EXISTS
        (SELECT 1 FROM sync_copies WHERE source=?1 AND thread=?2 AND message=m.id) ORDER BY m.id
      ON CONFLICT(source,direction,thread,event_key) DO UPDATE SET payload=excluded.payload,state='pending',attempts=0,available=0,error=NULL
      WHERE sync_queue.state='skipped' AND sync_queue.error='publishing disabled'",params![s.name,thread,root])?)
}
