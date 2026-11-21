//! Recovery of pre-v16 timers. This module has no path to create a post or reply.
use crate::Board;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::io::Write;

/// Provenance on a message posted before timers were retired.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ScheduledSend {
    pub id: i64,
    pub n: i64,
}

pub(super) fn migrate(c: &Connection) -> Result<()> {
    let archive = c.table_exists(None, "schedules")?;
    if archive {
        // Renaming the queue in the migration transaction also fences an already-open v15
        // worker: its next SELECT/INSERT cannot find the old table. Do not emit any events.
        for name in [
            "schedules",
            "schedule_sends",
            "schedule_files",
            "schedule_refs",
            "schedule_details",
            "local_only_schedules",
            "blind_schedules",
            "schedule_auth",
            "schedule_caps",
        ] {
            if c.table_exists(None, name)? {
                c.execute_batch(&format!("ALTER TABLE {name} RENAME TO legacy_{name}"))?;
            }
        }
        c.execute_batch("DROP INDEX IF EXISTS schedules_due")?;
    }
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS legacy_schedule_sends(
        message_id INTEGER PRIMARY KEY REFERENCES messages(id), schedule_id INTEGER NOT NULL,
        ordinal INTEGER NOT NULL, UNIQUE(schedule_id,ordinal));",
    )?;
    if archive {
        // Some pre-v15 schemas only recorded the original due time or send ID in the log.
        c.execute_batch("CREATE TABLE IF NOT EXISTS legacy_schedule_files(
                schedule INTEGER NOT NULL, ordinal INTEGER NOT NULL, name TEXT NOT NULL,
                data BLOB NOT NULL, PRIMARY KEY(schedule,ordinal));
            CREATE TABLE IF NOT EXISTS legacy_schedule_refs(
                schedule INTEGER NOT NULL, ordinal INTEGER NOT NULL, file INTEGER NOT NULL,
                name TEXT NOT NULL, size INTEGER NOT NULL, PRIMARY KEY(schedule,ordinal));
            CREATE TABLE IF NOT EXISTS legacy_schedule_details(
                schedule INTEGER PRIMARY KEY, first INTEGER, ended_by TEXT, reason TEXT);
            CREATE TABLE IF NOT EXISTS legacy_local_only_schedules(schedule INTEGER PRIMARY KEY);
            CREATE TABLE IF NOT EXISTS legacy_blind_schedules(schedule INTEGER PRIMARY KEY);
            INSERT OR IGNORE INTO legacy_schedule_details(schedule,first)
                SELECT s.id,(SELECT json_extract(e.payload,'$.due') FROM events e
                    WHERE e.kind='schedule.created' AND json_extract(e.payload,'$.id')=s.id
                    ORDER BY e.cursor LIMIT 1) FROM legacy_schedules s
                WHERE NOT EXISTS (SELECT 1 FROM legacy_schedule_details d WHERE d.schedule=s.id);
            INSERT OR IGNORE INTO legacy_schedule_sends(message_id,schedule_id,ordinal)
                SELECT m.id,s.id,json_extract(e.payload,'$.n') FROM events e
                JOIN messages m ON m.id=coalesce(e.message_id,json_extract(e.payload,'$.message_id'))
                JOIN legacy_schedules s ON s.id=json_extract(e.payload,'$.id')
                WHERE e.kind='schedule.sent' AND json_type(e.payload,'$.n')='integer';")?;
    }
    Ok(())
}

pub(crate) fn send_info(c: &Connection, message: i64) -> Result<Option<ScheduledSend>> {
    Ok(c.query_row(
        "SELECT schedule_id,ordinal FROM legacy_schedule_sends WHERE message_id=?",
        [message],
        |r| {
            Ok(ScheduledSend {
                id: r.get(0)?,
                n: r.get(1)?,
            })
        },
    )
    .optional()?)
}

/// Local-owner recovery only. Queued file bytes are hex; existing uploads retain their download IDs.
/// Never export the registration's credential or capabilities.
pub fn export_legacy_schedules(board: &Board, mut out: impl Write) -> Result<()> {
    let tx = board.conn.unchecked_transaction()?;
    board.access_on(&tx)?.trusted()?;
    out.write_all(br#"{"format":"board.retired-schedules.v1","schedules":["#)?;
    if tx.table_exists(None, "legacy_schedules")? {
        let mut stmt = tx.prepare("SELECT json_object(
            'id',s.id,'channel',s.channel,'re',s.re,'text',s.text,'actor',s.actor,'due',s.due,
            'first',d.first,'every',s.every,'max_sends',s.max_sends,'until',s.until_at,
            'sent',s.sent,'state_at_retirement',s.state,'ended_by',d.ended_by,'reason',d.reason,
            'labels',json(s.labels),
            'no_sync',json(CASE WHEN p.schedule IS NULL THEN 'false' ELSE 'true' END),
            'blind',json(CASE WHEN b.schedule IS NULL THEN 'false' ELSE 'true' END),
            'sends',(SELECT json_group_array(json_object('message_id',message_id,'n',ordinal) ORDER BY ordinal)
                FROM legacy_schedule_sends WHERE schedule_id=s.id),
            'attachments',(SELECT json_group_array(json(item) ORDER BY kind,ordinal) FROM (
                SELECT 0 AS kind,ordinal,json_object('name',name,'size',size,'attachment_id',file) AS item
                    FROM legacy_schedule_refs WHERE schedule=s.id
                UNION ALL SELECT 1,ordinal,json_object('name',name,'size',length(data),'data_hex',hex(data))
                    FROM legacy_schedule_files WHERE schedule=s.id)))
            FROM legacy_schedules s LEFT JOIN legacy_schedule_details d ON d.schedule=s.id
            LEFT JOIN legacy_local_only_schedules p ON p.schedule=s.id
            LEFT JOIN legacy_blind_schedules b ON b.schedule=s.id ORDER BY s.id")?;
        for (n, row) in stmt.query_map([], |r| r.get::<_, String>(0))?.enumerate() {
            if n > 0 {
                out.write_all(b",")?;
            }
            out.write_all(row?.as_bytes())?;
        }
    }
    out.write_all(b"]}\n")?;
    tx.commit()?;
    Ok(())
}
