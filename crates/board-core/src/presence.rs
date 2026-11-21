//! Short-lived status for local thread participants; never a board event or watcher wake.
use crate::{Board, access::Action};
use anyhow::{Result, ensure};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

pub(crate) fn init(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS typing_presence(root INTEGER NOT NULL REFERENCES messages(id),actor TEXT NOT NULL,status TEXT NOT NULL,expires INTEGER NOT NULL,PRIMARY KEY(root,actor));")?;
    Ok(())
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Typing {
    pub actor: String,
    pub status: String,
    pub expires: i64,
}
impl Board {
    pub fn set_typing(&self, id: i64, who: &str, status: Option<&str>, seconds: i64) -> Result<()> {
        ensure!(
            (1..=120).contains(&seconds),
            "typing lifetime must be 1..120 seconds"
        );
        crate::validate_actor(who)?;
        if let Some(t) = status {
            ensure!(
                t.chars().count() <= 160 && !t.chars().any(char::is_control),
                "invalid typing status"
            );
        }
        self.write(|tx|{let a=self.access_on(tx)?;a.actor(who)?;a.message(tx,id,Action::Post)?;let msg=Self::get_on(tx,id)?;let root=msg.re.unwrap_or(id);let now=chrono::Utc::now().timestamp();
            tx.execute("DELETE FROM typing_presence WHERE expires<=?",[now])?;
            if let Some(t)=status {tx.execute("INSERT INTO typing_presence VALUES(?,?,?,?) ON CONFLICT(root,actor) DO UPDATE SET status=excluded.status,expires=excluded.expires",params![root,who,t,now+seconds])?;}
            else{tx.execute("DELETE FROM typing_presence WHERE root=? AND actor=?",params![root,who])?;}Ok(())})
    }
    pub fn typing(&self, id: i64) -> Result<Vec<Typing>> {
        let tx = self.conn.unchecked_transaction()?;
        self.access_on(&tx)?.message(&tx, id, Action::Read)?;
        let msg = Self::get_on(&tx, id)?;
        let rows=tx.prepare("SELECT actor,status,expires FROM typing_presence WHERE root=? AND expires>? ORDER BY actor")?.query_map(params![msg.re.unwrap_or(id),chrono::Utc::now().timestamp()],|r|Ok(Typing{actor:r.get(0)?,status:r.get(1)?,expires:r.get(2)?}))?.collect::<rusqlite::Result<_>>()?;
        tx.commit()?;
        Ok(rows)
    }
}
