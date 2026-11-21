use super::*;

/// A local handler may mint output keys without being given its parent's acknowledgement receipt.
pub fn start_callback(b: &Board, name: &str, cursor: i64, receipt: &str) -> Result<String> {
    let tx = write(&b.conn)?;
    let (w, p) = Policies::open(b, &tx, name)?;
    let live:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM watcher_queue WHERE name=? AND cursor=? AND receipt=? AND lease_until>?)",params![w.key,cursor,receipt,now()],|r|r.get(0))?;
    ensure!(
        live && p.delivered(&tx, event_at(&tx, cursor)?)?.is_some(),
        Conflict.error("delivery_conflict", "not a live delivery")
    );
    let actor = w.run_as.context("watcher has no run-as actor")?;
    let token = uuid::Uuid::new_v4().to_string();
    let n=tx.execute("INSERT INTO watcher_callback_runs VALUES(?,?,?,?,?,0) ON CONFLICT(name,cursor) DO UPDATE SET receipt=excluded.receipt,capability=excluded.capability,seq=0 WHERE receipt<>excluded.receipt",
        params![w.key,cursor,receipt,Sha256::digest(token.as_bytes()).as_slice(),actor])?;
    ensure!(
        n == 1,
        Conflict.error(
            "delivery_conflict",
            "callback already started for this lease"
        )
    );
    tx.commit()?;
    Ok(token)
}

pub fn callback_key(b: &Board, actor: &str, token: &str) -> Result<String> {
    callback_write_key(b, actor, token, None)
}
pub fn callback_write_key(
    b: &Board,
    actor: &str,
    token: &str,
    given: Option<&str>,
) -> Result<String> {
    let tx = write(&b.conn)?;
    let a = b.access_on(&tx)?;
    a.actor(actor)?;
    let failure = || Conflict.error("delivery_conflict", "no live callback for this actor");
    let (name,cursor,seq,public):(String,i64,i64,String)=tx.query_row(
        "SELECT c.name,c.cursor,c.seq,coalesce(r.name,c.name) FROM watcher_callback_runs c
        JOIN watcher_queue q ON q.name=c.name AND q.cursor=c.cursor AND q.receipt=c.receipt
        LEFT JOIN watcher_registry r ON r.internal=c.name WHERE c.capability=? AND c.actor=? AND q.lease_until>?",
        params![Sha256::digest(token.as_bytes()).as_slice(),actor,now()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?.with_context(failure)?;
    let (w, p) = Policies::open(b, &tx, &public)?;
    ensure!(
        w.key == name && p.delivered(&tx, event_at(&tx, cursor)?)?.is_some(),
        failure()
    );
    let board = super::super::claim_namespace(b)?;
    let parts = if let Some(given) = given {
        crate::core::validate_client_key(given)?;
        serde_json::to_vec(&("named", board, actor, given))?
    } else {
        tx.execute(
            "UPDATE watcher_callback_runs SET seq=seq+1 WHERE name=? AND cursor=?",
            params![name, cursor],
        )?;
        serde_json::to_vec(&("automatic", board, actor, &name, cursor, seq))?
    };
    let key = format!("callback:{:x}", Sha256::digest(parts));
    tx.execute(
        "INSERT OR IGNORE INTO watcher_outputs VALUES(?,?)",
        params![key, actor],
    )?;
    tx.commit()?;
    Ok(key)
}

pub(crate) fn authorize_callback_key(c: &Connection, actor: &str, key: &str) -> Result<()> {
    if key
        .strip_prefix("callback:")
        .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        let issued: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM watcher_outputs WHERE key=? AND actor=?)",
            params![key, actor],
            |r| r.get(0),
        )?;
        ensure!(
            issued,
            Forbidden.error(
                "reserved_key",
                "callback keys are issued only to the handler's actor"
            )
        );
    }
    Ok(())
}
