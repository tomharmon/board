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
#[derive(Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    pub stream: String,
    pub expected: Option<String>,
    pub next: String,
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
    ident(&e.key)?;
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
            mapping::apply(tx, s, &serde_json::from_value(j.payload.clone())?)?
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
                tx.execute(
                    "INSERT INTO sync_file_copies VALUES(?,?,?,?,?,'out')",
                    params![s.name, target, r.external, m.id, asset],
                )?;
            } else {
                mapping::bind(tx, s, target, &r.external, &m, false, r.url.as_deref())?;
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

impl Board {
    /// Intentionally start a remote conversation from a local root; a stopped/offline connector does nothing.
    pub fn start_source_thread(&self, who: &str, name: &str, root: i64) -> Result<Job> {
        self.write(|tx|{let s=self.authorized_source(tx,name,who)?;ensure!(s.outbound,"publishing must be explicitly enabled");
            let m=Self::get_on(tx,root)?;ensure!(m.channel==s.channel&&m.re.is_none()&&!m.deleted&&!m.actor.starts_with("source:"),"expected a live local root in the mapped channel");
            if let Some(id)=tx.query_row("SELECT item FROM sync_starts WHERE source=? AND root=?",params![name,root],|r|r.get(0)).optional()?{return get(tx,name,id);}
            ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM sync_threads WHERE source=? AND root=?)",params![name,root],|r|r.get::<_,bool>(0))?,Kind::Conflict.error("already_linked","thread already linked at this provider"));
            let lane=format!("pending:{}",Uuid::new_v4());tx.execute("INSERT INTO sync_threads VALUES(?,?,?)",params![name,lane,root])?;
            let d=Delivery{event:"thread.start".into(),message:root,cursor:root,text:m.text,parent:None,emoji:None,asset:None};
            tx.execute("INSERT INTO sync_queue(source,direction,thread,event_key,payload,raw) VALUES(?,'out',?,?,?,'null')",params![name,lane,root.to_string(),serde_json::to_string(&d)?])?;
            let id=tx.last_insert_rowid();tx.execute("INSERT INTO sync_starts VALUES(?,?,?)",params![name,root,id])?;
            backfill(tx,&s,&lane,root)?;
            Self::event_tx(tx,"thread.sync_requested",who,Some(root),json!({"source":name,"item":id}))?;
            get(tx,name,id)})
    }
    pub fn backfill_source(&self, who: &str, name: &str, thread: &str) -> Result<usize> {
        self.write(|tx| {
            let s = self.authorized_source(tx, name, who)?;
            ensure!(s.outbound, "publishing must be explicitly enabled");
            let root = mapping::root_on(tx, name, thread)?.context("thread is not linked")?;
            backfill(tx, &s, thread, root)
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
            if j.state=="done"{return self.access_on(tx)?.view(tx,Self::get_on(tx,j.result.context("delivery has no result")?)?);}
            ensure!(j.state=="pending"&&!tx.query_row("SELECT EXISTS(SELECT 1 FROM sync_queue WHERE source=? AND direction='in' AND thread=? AND id<? AND state NOT IN ('done','failed','skipped'))",params![name,j.thread,id],|r|r.get::<_,bool>(0))?,Kind::Conflict.error("lane_busy","an earlier delivery or lease is outstanding"));
            self.access_on(tx)?.view(tx,complete(tx,&s,&j,None)?)})
    }
    pub fn inbox(
        &self,
        who: &str,
        name: &str,
        dir: Direction,
        after: i64,
        limit: usize,
    ) -> Result<Vec<Job>> {
        ensure!(
            after >= 0 && (1..=500).contains(&limit),
            "invalid cursor or limit"
        );
        let tx = self.conn.unchecked_transaction()?;
        self.authorized_source(&tx, name, who)?;
        let rows=tx.prepare(&format!("SELECT {COLS} FROM sync_queue WHERE source=? AND direction=? AND id>? ORDER BY id LIMIT ?"))?.query_map(params![name,dir.sql(),after,limit as i64],item)?.collect::<rusqlite::Result<_>>()?;
        tx.commit()?;
        Ok(rows)
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
        ensure!(
            (1..=3600).contains(&seconds),
            "lease must be 1..3600 seconds"
        );
        self.write(|tx|{let s=self.authorized_source(tx,name,who)?;let now=time();expire(tx,name,now)?;
            if dir==Direction::Out && !s.outbound {return Ok(None);}
            let id:Option<i64>=tx.query_row("SELECT q.id FROM sync_queue q WHERE source=? AND direction=? AND state='pending' AND available<=?
                AND (q.direction!='in' OR NOT EXISTS(SELECT 1 FROM sync_queue o WHERE o.source=q.source AND o.direction='out' AND o.state IN ('sending','uncertain') AND (o.thread=q.thread OR json_extract(o.payload,'$.event')='thread.start')))
                AND NOT EXISTS(SELECT 1 FROM sync_queue h WHERE h.source=q.source AND h.direction=q.direction AND h.thread=q.thread AND h.id<q.id AND h.state NOT IN ('done','failed','skipped')) ORDER BY q.id LIMIT 1",
                params![name,dir.sql(),now],|r|r.get(0)).optional()?;
            let Some(id)=id else{return Ok(None)};let receipt=Uuid::new_v4().to_string();let deadline=now+seconds;
            tx.execute("UPDATE sync_queue SET state='leased',attempts=attempts+1,receipt=?,deadline=? WHERE id=?",params![receipt,deadline,id])?;
            Ok(Some(Lease{job:get(tx,name,id)?,receipt,deadline}))})
    }
    /// Commit the intent before an external write. A crash after this point never silently repeats a remote POST.
    pub fn start_sending(&self, who: &str, name: &str, id: i64, receipt: &str) -> Result<()> {
        self.write(|tx| {
            let s = self.authorized_source(tx, name, who)?;
            let j = check_receipt(tx, name, id, receipt)?;
            ensure!(
                s.outbound && j.direction == Direction::Out && j.state == "leased",
                Kind::Conflict.error(
                    "not_sendable",
                    "publishing is disabled or the delivery is not newly leased"
                )
            );
            tx.execute("UPDATE sync_queue SET state='sending' WHERE id=?", [id])?;
            Ok(())
        })
    }
    /// Hand an external consumer a lease. Returning an outbound receipt arms crash reconciliation.
    pub fn take_source_delivery(
        &self,
        who: &str,
        name: &str,
        dir: Direction,
        seconds: i64,
    ) -> Result<Option<Lease>> {
        let Some(mut l) = self.lease_source(who, name, dir, seconds)? else {
            return Ok(None);
        };
        if dir == Direction::Out {
            self.start_sending(who, name, l.job.id, &l.receipt)?;
            l.job.state = "sending".into();
        }
        Ok(Some(l))
    }
    pub fn finish_source(
        &self,
        who: &str,
        name: &str,
        id: i64,
        receipt: &str,
        result: Option<&DeliveryResult>,
    ) -> Result<Message> {
        self.write(|tx| {
            let s = self.authorized_source(tx, name, who)?;
            let j = check_receipt(tx, name, id, receipt)?;
            ensure!(
                j.direction == Direction::In || j.state == "sending",
                Kind::Conflict.error("not_sending", "record the external send intent first")
            );
            self.access_on(tx)?.view(tx, complete(tx, &s, &j, result)?)
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
                "delivered"=>{ensure!(j.direction==Direction::Out,"inbound events cannot have remote receipts");complete(tx,&s,&j,confirmed)?;}
                "retry"|"skip"=>{ensure!(confirmed.is_none(),"unexpected receipt");ensure!(action=="skip"||j.direction==Direction::In||s.outbound,"publishing is disabled");
                    tx.execute("UPDATE sync_queue SET state=?,attempts=0,available=0,error=NULL WHERE id=?",params![if action=="retry"{"pending"}else{"skipped"},id])?;
                    Board::event_tx(tx,"source.reconciled",who,None,json!({"source":name,"channel":s.channel,"item":id,"action":action}))?;}
                _=>anyhow::bail!("expected delivered, retry or skip"),
            }get(tx,name,id)})
    }
    /// Drain stored input only. Never opens a socket.
    pub fn drain_source(&self, who: &str, name: &str, limit: usize) -> Result<(usize, usize)> {
        ensure!((1..=10000).contains(&limit), "limit must be 1..10000");
        let (mut ok, mut failed) = (0, 0);
        for _ in 0..limit {
            let Some(l) = self.lease_source(who, name, Direction::In, 60)? else {
                break;
            };
            match self.finish_source(who, name, l.job.id, &l.receipt, None) {
                Ok(_) => ok += 1,
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
                    failed += 1;
                }
            }
        }
        Ok((ok, failed))
    }
}

fn backfill(tx: &Transaction<'_>, s: &Source, thread: &str, root: i64) -> Result<usize> {
    Ok(tx.execute("INSERT OR IGNORE INTO sync_queue(source,direction,thread,event_key,payload,raw)
      SELECT ?1,'out',?2,CAST(m.id AS TEXT),json_object('event','message.posted','message',m.id,'cursor',m.id,'text',m.text,'parent',m.reply_to),'null'
      FROM messages m WHERE m.re=?3 AND m.deleted=0 AND substr(m.actor,1,7)!='source:' AND NOT EXISTS
        (SELECT 1 FROM sync_copies WHERE source=?1 AND thread=?2 AND message=m.id) ORDER BY m.id",params![s.name,thread,root])?)
}
