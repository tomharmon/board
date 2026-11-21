use super::{now, write};
use crate::access::Action;
use crate::{Board, PostOptions, validate_post};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

#[derive(Debug, Serialize)]
pub struct Schedule {
    pub id: i64,
    pub channel: String,
    pub re: Option<i64>,
    pub text: String,
    pub actor: String,
    pub due: i64,
    /// An older database may not have recorded a creation event.
    pub first: Option<i64>,
    pub next: Option<i64>,
    pub last: Option<i64>,
    pub canceled: Option<ScheduleEnd>,
    pub every: Option<i64>,
    pub max_sends: Option<i64>,
    pub until: Option<i64>,
    pub sent: i64,
    pub state: String,
    pub no_sync: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<crate::MentionResolution>,
    pub labels: BTreeMap<String, String>,
    pub attachments: Vec<ScheduledFile>,
    #[serde(skip)]
    blind: bool,
}
#[derive(Debug, Serialize)]
pub struct ScheduleEnd {
    pub by: String,
    pub reason: String,
}
#[derive(Debug, Serialize)]
pub struct ScheduledFile {
    pub name: String,
    pub size: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
}
pub struct Timing {
    pub due: i64,
    pub every: Option<i64>,
    pub times: Option<i64>,
    pub until: Option<i64>,
}
pub enum Destination<'a> {
    Channel(&'a str),
    Reply(i64),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ScheduledSend {
    pub id: i64,
    pub n: i64,
}

pub(super) fn init(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schedule_sends(
            message_id INTEGER PRIMARY KEY REFERENCES messages(id),
            schedule_id INTEGER NOT NULL REFERENCES schedules(id),
            ordinal INTEGER NOT NULL,
            UNIQUE(schedule_id,ordinal)
        );
        CREATE TABLE IF NOT EXISTS schedule_files(
            schedule INTEGER NOT NULL REFERENCES schedules(id), ordinal INTEGER NOT NULL,
            name TEXT NOT NULL,data BLOB NOT NULL, PRIMARY KEY(schedule,ordinal));
        CREATE TABLE IF NOT EXISTS schedule_auth(schedule INTEGER PRIMARY KEY REFERENCES schedules(id),credential TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS schedule_caps(schedule INTEGER PRIMARY KEY REFERENCES schedules(id),rules TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS blind_schedules(schedule INTEGER PRIMARY KEY REFERENCES schedules(id));
        CREATE TABLE IF NOT EXISTS local_only_schedules(schedule INTEGER PRIMARY KEY REFERENCES schedules(id));
        CREATE TABLE IF NOT EXISTS schedule_details(schedule INTEGER PRIMARY KEY REFERENCES schedules(id),first INTEGER,ended_by TEXT,reason TEXT);
        INSERT OR IGNORE INTO schedule_details(schedule,first)
            SELECT s.id,(SELECT json_extract(e.payload,'$.due') FROM events e WHERE e.kind='schedule.created' AND json_extract(e.payload,'$.id')=s.id ORDER BY e.cursor LIMIT 1) FROM schedules s;
        CREATE TABLE IF NOT EXISTS schedule_refs(schedule INTEGER NOT NULL REFERENCES schedules(id),ordinal INTEGER NOT NULL,file INTEGER NOT NULL,name TEXT NOT NULL,size INTEGER NOT NULL,PRIMARY KEY(schedule,ordinal));
        INSERT OR IGNORE INTO schedule_sends(message_id,schedule_id,ordinal)
            SELECT m.id,s.id,json_extract(e.payload,'$.n') FROM events e
            JOIN messages m ON m.id=e.message_id
            JOIN schedules s ON s.id=json_extract(e.payload,'$.id')
            WHERE e.kind='schedule.sent' AND json_type(e.payload,'$.n')='integer';",
    )?;
    Ok(())
}

pub(crate) fn send_info(conn: &Connection, message: i64) -> Result<Option<ScheduledSend>> {
    Ok(conn
        .query_row(
            "SELECT schedule_id,ordinal FROM schedule_sends WHERE message_id=?",
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

fn get(conn: &Connection, id: i64) -> Result<Schedule> {
    let (mut s, labels) = conn
        .query_row(
            "SELECT channel,re,text,actor,due,every,max_sends,until_at,sent,state,labels
        FROM schedules WHERE id=?",
            [id],
            |r| {
                Ok((
                    Schedule {
                        id,
                        channel: r.get(0)?,
                        re: r.get(1)?,
                        text: r.get(2)?,
                        actor: r.get(3)?,
                        due: r.get(4)?,
                        first: None,
                        next: None,
                        last: None,
                        canceled: None,
                        every: r.get(5)?,
                        max_sends: r.get(6)?,
                        until: r.get(7)?,
                        sent: r.get(8)?,
                        state: r.get(9)?,
                        no_sync: false,
                        mentions: vec![],
                        labels: BTreeMap::new(),
                        attachments: Vec::new(),
                        blind: false,
                    },
                    r.get::<_, String>(10)?,
                ))
            },
        )
        .optional()?
        .context(crate::errors::Kind::Missing.error("schedule_not_found", "schedule not found"))?;
    s.labels = serde_json::from_str(&labels)?;
    (s.first, s.canceled) = conn
        .query_row(
            "SELECT first,ended_by,reason FROM schedule_details WHERE schedule=?",
            [id],
            |r| {
                let by: Option<String> = r.get(1)?;
                let why: Option<String> = r.get(2)?;
                Ok((
                    r.get(0)?,
                    by.zip(why).map(|(by, reason)| ScheduleEnd { by, reason }),
                ))
            },
        )
        .optional()?
        .unwrap_or((None, None));
    s.next = (s.state == "pending").then_some(s.due);
    s.last=conn.query_row("SELECT message_id FROM schedule_sends WHERE schedule_id=? ORDER BY ordinal DESC LIMIT 1",[id],|r|r.get(0)).optional()?;
    s.no_sync = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM local_only_schedules WHERE schedule=?)",
        [id],
        |r| r.get(0),
    )?;
    s.blind = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM blind_schedules WHERE schedule=?)",
        [id],
        |r| r.get(0),
    )?;
    s.attachments = conn
        .prepare("SELECT name,size,file FROM (SELECT name,size,file,0 AS kind,ordinal FROM schedule_refs WHERE schedule=?1
            UNION ALL SELECT name,length(data),NULL,1,ordinal FROM schedule_files WHERE schedule=?1) ORDER BY kind,ordinal")?
        .query_map([id], |r| {
            Ok(ScheduledFile {
                name: r.get(0)?,
                size: r.get(1)?,
                id:r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(s)
}

pub fn schedule(
    board: &Board,
    actor: &str,
    target: Destination<'_>,
    text: &str,
    t: Timing,
    labels: BTreeMap<String, String>,
) -> Result<Schedule> {
    schedule_post(
        board,
        actor,
        target,
        text,
        t,
        &PostOptions {
            labels,
            ..Default::default()
        },
    )
}

pub fn schedule_post(
    board: &Board,
    actor: &str,
    target: Destination<'_>,
    text: &str,
    t: Timing,
    options: &PostOptions,
) -> Result<Schedule> {
    schedule_inner(board, actor, target, text, t, options, None)
}

/// Retire the exact saved composer draft in the transaction that accepts its schedule.
pub fn schedule_draft(
    board: &Board,
    actor: &str,
    target: Destination<'_>,
    text: &str,
    t: Timing,
    options: &PostOptions,
    guard: (&str, &str),
) -> Result<Schedule> {
    schedule_inner(board, actor, target, text, t, options, Some(guard))
}
fn schedule_inner(
    board: &Board,
    actor: &str,
    target: Destination<'_>,
    text: &str,
    t: Timing,
    options: &PostOptions,
    draft: Option<(&str, &str)>,
) -> Result<Schedule> {
    ensure!(
        options.after.is_none() && options.key.is_none(),
        "schedule does not take an idempotency or latest-message condition"
    );
    let year = 365 * 24 * 60 * 60;
    let current = now();
    ensure!(
        t.due >= current - 60,
        "first send is more than a minute in the past"
    );
    ensure!(
        t.due <= current + year,
        "first send is more than a year away"
    );
    ensure!(
        t.every.is_none_or(|e| (60..=year).contains(&e)),
        "repeat interval must be one minute to one year"
    );
    ensure!(
        t.times.is_none_or(|n| n > 0 && t.every.is_some()),
        "positive --times requires a repeat"
    );
    ensure!(
        t.times.is_none() || t.until.is_none(),
        "set only one repeat stop: times or until"
    );
    ensure!(
        t.until
            .is_none_or(|u| u >= t.due && u <= current + year && t.every.is_some()),
        "invalid stop time"
    );
    let tx = write(&board.conn)?;
    let access = board.access_on(&tx)?;
    access.actor(actor)?;
    let (channel, re) = match target {
        Destination::Channel(s) => (s.to_owned(), None),
        Destination::Reply(id) => {
            access.message(&tx, id, Action::Read)?;
            let m = Board::get_on(&tx, id)?;
            let root = m.re.unwrap_or(m.id);
            ensure!(
                !m.deleted && !Board::get_on(&tx, root)?.deleted,
                crate::errors::Kind::Gone.error("deleted", "thread is deleted")
            );
            (m.channel, Some(m.id))
        }
    };
    access.require(Action::Post, &channel)?;
    if let Some((scope, revision)) = draft {
        let root = if let Some(id) = re {
            Some(Board::get_on(&tx, id)?.re.unwrap_or(id))
        } else {
            None
        };
        let mismatch = || {
            crate::errors::Kind::Conflict
                .error("draft_changed", "draft was already sent, edited, or moved")
        };
        ensure!(
            root.map_or_else(
                || crate::drafts::root_scope(scope, &channel),
                |id| scope == format!("thread:{id}")
            ),
            mismatch()
        );
        let saved = board.check_composer_options_on(&tx, actor, scope, text, options, Some(&t))?;
        ensure!(
            saved.channel == channel && saved.reply_to.or(root) == re,
            mismatch()
        );
        board.consume_composer_on(&tx, actor, scope, revision)?;
    }
    if !options.labels.is_empty() {
        access.require(Action::Label, &channel)?;
    }
    let (text, mentions) = crate::actors::settle(&tx, &access, actor, re, text)?;
    validate_post(&channel, actor, &text, options)?;
    access.files(&tx, actor, &options.attachment_ids)?;
    Board::blocks_on(&tx, &text)?;
    tx.execute(
        "INSERT INTO schedules(channel,re,text,actor,due,every,max_sends,until_at,labels)
        VALUES(?,?,?,?,?,?,?,?,?)",
        params![
            channel,
            re,
            text,
            actor,
            t.due,
            t.every,
            t.times,
            t.until,
            serde_json::to_string(&options.labels)?
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO schedule_details(schedule,first) VALUES(?,?)",
        [id, t.due],
    )?;
    if options.no_sync {
        tx.execute("INSERT INTO local_only_schedules VALUES(?)", [id])?;
    }
    if !access.allows(Action::Read, &channel) {
        tx.execute("INSERT INTO blind_schedules VALUES(?)", [id])?;
    }
    for (ordinal, (name, data)) in options.files.iter().enumerate() {
        tx.execute(
            "INSERT INTO schedule_files VALUES(?,?,?,?)",
            params![id, ordinal as i64, name, data],
        )?;
    }
    for (ord, file) in options.attachment_ids.iter().enumerate() {
        let m = Board::attachment_on(&tx, *file)?;
        tx.execute(
            "INSERT INTO schedule_refs VALUES(?,?,?,?,?)",
            params![id, ord as i64, file, m.name, m.size],
        )?;
    }
    if let Some(credential) = access.credential_id() {
        tx.execute(
            "INSERT INTO schedule_auth VALUES(?,?)",
            params![id, credential],
        )?;
    }
    if let Some(cap) = access.durable_cap() {
        tx.execute(
            "INSERT INTO schedule_caps VALUES(?,?)",
            params![id, serde_json::to_string(cap)?],
        )?;
    }
    let mut s = get(&tx, id)?;
    s.mentions = mentions;
    Board::event_tx(
        &tx,
        "schedule.created",
        actor,
        None,
        json!({"id":s.id, "due":s.due, "channel":s.channel}),
    )?;
    tx.commit()?;
    Ok(s)
}

pub fn schedules(board: &Board, all: bool) -> Result<Vec<Schedule>> {
    Ok(schedule_query(board, all, None, None, 0, i64::MAX, false)?.1)
}

pub fn get_schedule(board: &Board, id: i64) -> Result<Schedule> {
    let tx = board.conn.unchecked_transaction()?;
    let s = get(&tx, id)?;
    let a = board.access_on(&tx)?;
    ensure!(
        can_view(&a, &s.channel, &s.actor, s.blind),
        crate::errors::Kind::Missing.error("schedule_not_found", "schedule not found")
    );
    tx.commit()?;
    Ok(s)
}

fn can_view(a: &crate::access::Access, channel: &str, owner: &str, blind: bool) -> bool {
    a.allows(Action::Read, channel)
        || (blind
            && a.allows(Action::Post, channel)
            && a.bound_actor()
                .is_some_and(|actor| crate::owned_by(owner, actor)))
}

pub(crate) fn pending_in_thread(conn: &Connection, root: i64) -> Result<Vec<Schedule>> {
    conn.prepare(
        "SELECT s.id FROM schedules s JOIN messages m ON s.re=m.id
        WHERE s.state='pending' AND (m.id=?1 OR m.re=?1) ORDER BY s.due,s.id",
    )?
    .query_map([root], |row| row.get(0))?
    .map(|id| get(conn, id?))
    .collect()
}

/// REST collection pages use immutable IDs; a due time can change between requests.
pub fn schedules_page(
    board: &Board,
    all: bool,
    after: i64,
    limit: usize,
) -> Result<(i64, Vec<Schedule>)> {
    ensure!(
        after >= 0 && (1..=501).contains(&limit),
        "invalid cursor or limit"
    );
    schedule_query(board, all, None, None, after, limit as i64, true)
}

pub fn schedule_query(
    board: &Board,
    all: bool,
    thread: Option<i64>,
    by: Option<&str>,
    after: i64,
    limit: i64,
    by_id: bool,
) -> Result<(i64, Vec<Schedule>)> {
    schedule_query_in(board, all, thread, by, None, after, limit, by_id)
}

/// An exact channel is selected by name; a trailing slash includes its subtree.
#[allow(clippy::too_many_arguments)]
pub fn schedule_query_in(
    board: &Board,
    all: bool,
    thread: Option<i64>,
    by: Option<&str>,
    channel: Option<&str>,
    after: i64,
    limit: i64,
    by_id: bool,
) -> Result<(i64, Vec<Schedule>)> {
    ensure!(after >= 0 && limit > 0, "invalid schedule range");
    let base = channel.map(|ch| ch.strip_suffix('/').unwrap_or(ch));
    if let Some(base) = base {
        crate::validate_channel(base)?;
    }
    let tx = board.conn.unchecked_transaction()?;
    let access = board.access_on(&tx)?;
    let authors = by
        .map(|query| crate::actors::author_filter(&tx, &access, query))
        .transpose()?;
    let root = if let Some(id) = thread {
        access.message(&tx, id, Action::Read)?;
        let m = Board::get_on(&tx, id)?;
        Some(m.re.unwrap_or(id))
    } else {
        None
    };
    let cursor = crate::read::cursor_on(&tx)?;
    let mut rows = Vec::new();
    for row in tx
        .prepare(
            "SELECT s.id,s.channel,s.actor,EXISTS(SELECT 1 FROM blind_schedules b WHERE b.schedule=s.id)
            FROM schedules s LEFT JOIN messages m ON m.id=s.re WHERE (?1 OR s.state='pending') AND s.id>?2
            AND (?4 IS NULL OR coalesce(m.re,m.id)=?4) AND (?5 IS NULL OR s.channel=?5 OR (?6!=?5 AND instr(s.channel,?6)=1))
            ORDER BY CASE WHEN ?3 THEN s.id ELSE s.due END,s.id",
        )?
        .query_map(params![all,after,by_id,root,base,channel],|r|Ok((r.get(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get(3)?)))?
    {
        let (id,c,owner,blind)=row?;
        if can_view(&access,&c,&owner,blind) && authors.as_ref().is_none_or(|a|a.matches(&owner)){
            rows.push(get(&tx,id)?);
            if rows.len() as i64>=limit{break;}
        }
    }
    tx.commit()?;
    Ok((cursor, rows))
}

pub fn cancel_schedule(board: &Board, id: i64, actor: &str) -> Result<Schedule> {
    let tx = write(&board.conn)?;
    let s = get(&tx, id)?;
    let access = board.access_on(&tx)?;
    access.actor(actor)?;
    ensure!(
        can_view(&access, &s.channel, &s.actor, s.blind),
        crate::errors::Kind::Missing.error("schedule_not_found", "schedule not found")
    );
    access.require(Action::Post, &s.channel)?;
    ensure!(
        (!access.is_trusted() && access.allows(Action::Manage, &s.channel))
            || s.actor == actor
            || s.actor
                .split_once('/')
                .is_some_and(|(person, _)| person == actor),
        crate::errors::Kind::Forbidden
            .error("not_owned", format!("schedule belongs to {}", s.actor))
    );
    if tx.execute(
        "UPDATE schedules SET state='canceled' WHERE id=? AND state='pending'",
        [id],
    )? != 0
    {
        tx.execute(
            "UPDATE schedule_details SET ended_by=?,reason='canceled' WHERE schedule=?",
            params![actor, id],
        )?;
        Board::event_tx(&tx, "schedule.canceled", actor, None, json!({"id":id}))?;
    }
    let s = get(&tx, id)?;
    tx.commit()?;
    Ok(s)
}

pub(crate) fn cancel_for_root(tx: &rusqlite::Transaction<'_>, id: i64, actor: &str) -> Result<()> {
    // A schedule may point at an individual reply, not only the thread root.
    let ids = tx
        .prepare(
            "SELECT id FROM schedules WHERE state='pending' AND
                          (re=?1 OR re IN (SELECT id FROM messages WHERE re=?1)) ORDER BY id",
        )?
        .query_map([id], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for schedule in ids {
        tx.execute(
            "UPDATE schedules SET state='canceled' WHERE id=?",
            [schedule],
        )?;
        tx.execute(
            "UPDATE schedule_details SET ended_by=?,reason='thread_deleted' WHERE schedule=?",
            params![actor, schedule],
        )?;
        Board::event_tx(
            tx,
            "schedule.canceled",
            actor,
            None,
            json!({"id":schedule,"deleted_target":id}),
        )?;
    }
    Ok(())
}

/// Each post and its schedule advancement commit together. Missed repeat slots coalesce.
pub fn dispatch_due(board: &Board) -> Result<usize> {
    board.require_owner()?;
    let mut count = 0;
    loop {
        let tx = write(&board.conn)?;
        let at = now();
        let id: Option<i64> = tx
            .query_row(
                "SELECT id FROM schedules WHERE state='pending' AND due<=?
            ORDER BY due,id LIMIT 1",
                [at],
                |r| r.get(0),
            )
            .optional()?;
        let Some(id) = id else {
            tx.commit()?;
            return Ok(count);
        };
        let s = get(&tx, id)?;
        let file_ids = s
            .attachments
            .iter()
            .filter_map(|f| f.id)
            .collect::<Vec<_>>();
        let credential: Option<String> = tx
            .query_row(
                "SELECT credential FROM schedule_auth WHERE schedule=?",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        let cap: Option<String> = tx
            .query_row(
                "SELECT rules FROM schedule_caps WHERE schedule=?",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        let access = match credential {
            Some(c) => Board::credential_access_on(&tx, &c)?,
            None => Some(board.access_on(&tx)?),
        };
        let access = match (access, cap) {
            (Some(a), Some(cap)) => Some(a.with_durable_cap(&s.actor, serde_json::from_str(&cap)?)),
            (a, None) => a,
            (None, Some(_)) => None,
        };
        let blind_send = access
            .as_ref()
            .is_some_and(|a| !a.allows(Action::Read, &s.channel));
        let allowed = if let Some(a) = access {
            let files = match a.files(&tx, &s.actor, &file_ids) {
                Ok(()) => true,
                Err(e)
                    if matches!(
                        crate::errors::classify(&e).0,
                        crate::errors::Kind::Missing | crate::errors::Kind::Forbidden
                    ) =>
                {
                    false
                }
                Err(e) => return Err(e),
            };
            files
                && a.actor(&s.actor).is_ok()
                && a.allows(Action::Post, &s.channel)
                && (s.re.is_none() || !blind_send)
                && (s.labels.is_empty() || a.allows(Action::Label, &s.channel))
        } else {
            false
        };
        if !allowed {
            tx.execute("UPDATE schedules SET state='revoked' WHERE id=?", [id])?;
            tx.execute(
                "UPDATE schedule_details SET ended_by='board',reason='revoked' WHERE schedule=?",
                [id],
            )?;
            Board::event_tx(
                &tx,
                "schedule.ended",
                &s.actor,
                None,
                json!({"id":id,"reason":"revoked"}),
            )?;
            tx.commit()?;
            continue;
        }
        if s.until.is_some_and(|until| at > until) {
            tx.execute("UPDATE schedules SET state='done' WHERE id=?", [id])?;
            Board::event_tx(
                &tx,
                "schedule.ended",
                &s.actor,
                None,
                json!({"id":id,"reason":"until"}),
            )?;
            tx.commit()?;
            continue;
        }
        let opts = PostOptions {
            no_sync: s.no_sync,
            labels: s.labels,
            attachment_ids: file_ids,
            files: tx
                .prepare("SELECT name,data FROM schedule_files WHERE schedule=? ORDER BY ordinal")?
                .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?,
            ..Default::default()
        };
        // A later schema change can invalidate a pending message. Don't stall other schedules.
        if Board::blocks_on(&tx, &s.text).is_err() {
            tx.execute("UPDATE schedules SET state='invalid' WHERE id=?", [id])?;
            Board::event_tx(
                &tx,
                "schedule.ended",
                &s.actor,
                None,
                json!({"id":id,"reason":"invalid schema block"}),
            )?;
            tx.commit()?;
            continue;
        }
        tx.execute_batch("SAVEPOINT scheduled_send")?;
        let attempted = match s.re {
            Some(re) => Board::reply_tx(&tx, re, &s.actor, &s.text, &opts),
            None => Board::post_tx(&tx, &s.channel, &s.actor, &s.text, &opts),
        };
        let post = match attempted {
            Ok(post) => {
                tx.execute_batch("RELEASE scheduled_send")?;
                post
            }
            Err(e) if crate::errors::classify(&e).1 == "dropped" => {
                tx.execute_batch("ROLLBACK TO scheduled_send; RELEASE scheduled_send")?;
                tx.execute("UPDATE schedules SET state='dropped' WHERE id=?", [id])?;
                tx.execute("UPDATE schedule_details SET ended_by='board',reason='dropped' WHERE schedule=?",[id])?;
                Board::event_tx(
                    &tx,
                    "schedule.ended",
                    &s.actor,
                    None,
                    json!({"id":id,"reason":"dropped"}),
                )?;
                tx.commit()?;
                continue;
            }
            Err(e) => return Err(e),
        };
        if blind_send {
            tx.execute("INSERT OR IGNORE INTO blind_posts(id) VALUES(?)", [post.id])?;
        }
        tx.execute(
            "INSERT INTO schedule_sends(message_id,schedule_id,ordinal) VALUES(?,?,?)",
            params![post.id, id, s.sent],
        )?;
        let tag = ScheduledSend { id, n: s.sent };
        tx.execute(
            "UPDATE events SET payload=? WHERE cursor=?",
            params![json!({"schedule":tag}).to_string(), post.id],
        )?;
        Board::snapshot(&tx, post.id, &Board::get_on(&tx, post.id)?, None)?;
        Board::event_tx(
            &tx,
            "schedule.sent",
            &s.actor,
            None,
            json!({"id":id,"n":s.sent,"message_id":post.id}),
        )?;
        let next = s.every.and_then(|e| {
            let elapsed = i128::from(at) - i128::from(s.due);
            at.checked_add(e - (elapsed % i128::from(e)) as i64)
        });
        let done = next.is_none()
            || s.max_sends.is_some_and(|n| s.sent + 1 >= n)
            || next.zip(s.until).is_some_and(|(due, end)| due > end);
        tx.execute(
            "UPDATE schedules SET sent=sent+1,due=?,state=? WHERE id=?",
            params![
                next.unwrap_or(s.due),
                if done { "done" } else { "pending" },
                id
            ],
        )?;
        tx.commit()?;
        count += 1;
    }
}
