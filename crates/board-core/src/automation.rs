//! Durable, local coordination over the same log and SQLite write transactions as messages.
use anyhow::Result;
use rusqlite::{Connection, Transaction, TransactionBehavior};

mod retired;
mod tasks;
#[cfg(test)]
mod tests;
mod watchers;
pub(crate) use retired::send_info;
pub use retired::{ScheduledSend, export_legacy_schedules};
pub use tasks::*;
pub use watchers::*;

fn write(conn: &Connection) -> Result<Transaction<'_>> {
    Ok(Transaction::new_unchecked(
        conn,
        TransactionBehavior::Immediate,
    )?)
}

pub(crate) fn init_automation(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS tasks(
            id INTEGER PRIMARY KEY REFERENCES messages(id), owner TEXT NOT NULL,
            assignee TEXT, state TEXT NOT NULL, holder TEXT, epoch INTEGER NOT NULL DEFAULT 0,
            lease_until INTEGER, result TEXT);
        CREATE INDEX IF NOT EXISTS tasks_queue ON tasks(state,id);
        CREATE TABLE IF NOT EXISTS task_needs(
            task INTEGER NOT NULL REFERENCES tasks(id), need INTEGER NOT NULL REFERENCES tasks(id),
            PRIMARY KEY(task,need), CHECK (task != need));
        CREATE INDEX IF NOT EXISTS task_dependents ON task_needs(need,task);
        CREATE TABLE IF NOT EXISTS task_requests(
            actor TEXT NOT NULL, key TEXT NOT NULL, fingerprint BLOB NOT NULL,
            ids TEXT NOT NULL, PRIMARY KEY(actor,key));
        CREATE TABLE IF NOT EXISTS task_claim_keys(
            task INTEGER PRIMARY KEY REFERENCES tasks(id), digest BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS task_receipts(
            digest BLOB PRIMARY KEY, actor TEXT NOT NULL, task INTEGER NOT NULL REFERENCES tasks(id),
            epoch INTEGER NOT NULL, target BLOB NOT NULL);
        INSERT OR IGNORE INTO task_receipts SELECT k.digest,t.holder,t.id,t.epoch,X''
            FROM task_claim_keys k JOIN tasks t ON t.id=k.task WHERE t.state='claimed' AND t.holder IS NOT NULL;
        CREATE TABLE IF NOT EXISTS task_holds(task INTEGER PRIMARY KEY REFERENCES tasks(id),kind TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS blind_tasks(task INTEGER PRIMARY KEY REFERENCES tasks(id),actor TEXT NOT NULL,original TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS plan_progress(key TEXT NOT NULL,position INTEGER NOT NULL,digest BLOB NOT NULL,task INTEGER NOT NULL REFERENCES tasks(id),PRIMARY KEY(key,position));
        INSERT OR IGNORE INTO task_holds SELECT id,'edited' FROM tasks WHERE state='blocked' AND result='root edited by another actor';
        CREATE TABLE IF NOT EXISTS claim_namespace(id INTEGER PRIMARY KEY CHECK(id=1),value TEXT NOT NULL);
        INSERT OR IGNORE INTO claim_namespace VALUES(1,lower(hex(randomblob(16))));
        CREATE TABLE IF NOT EXISTS watchers(
            name TEXT PRIMARY KEY, filter TEXT NOT NULL, kind TEXT, cursor INTEGER NOT NULL);",
    )?;
    // Before task views separated creator from person, snapshots retained the
    // exact creating actor under owner. Both values are recoverable without
    // consulting current task state or changing log positions.
    for column in ["message", "previous", "payload"] {
        let old = format!("json_extract({column},'$.task.owner')");
        conn.execute(&format!("UPDATE events SET {column}=json_set({column},'$.task.by',{old},'$.task.owner',substr({old},1,instr({old}||'/','/')-1))
            WHERE json_type({column},'$.task.owner')='text' AND json_type({column},'$.task.by') IS NULL"),[])?;
    }
    retired::migrate(conn)?;
    watchers::init(conn)?;
    Ok(())
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
