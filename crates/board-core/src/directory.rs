use crate::{Board, Channel, validate_channel};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, Row, params};

const COUNTS: &str =
    "SELECT c.name, count(m.id) FILTER (WHERE m.re IS NULL), count(m.id), max(m.id)
    FROM channels c LEFT JOIN messages m ON c.name=m.channel";

fn channel(row: &Row<'_>) -> rusqlite::Result<Channel> {
    Ok(Channel {
        name: row.get(0)?,
        threads: row.get(1)?,
        messages: row.get(2)?,
        last: row.get(3)?,
    })
}

impl Board {
    /// Private last-read message IDs; like the activity checkpoint, these are not board events.
    pub fn channel_read_marks(
        &self,
        actor: &str,
    ) -> Result<std::collections::BTreeMap<String, i64>> {
        let tx = self.conn.unchecked_transaction()?;
        let access = self.access_on(&tx)?;
        let key = access.private_key(actor)?;
        let mut marks = std::collections::BTreeMap::new();
        for row in tx
            .prepare("SELECT channel,message FROM channel_seen WHERE actor=?")?
            .query_map([key], |r| Ok((r.get::<_, String>(0)?, r.get(1)?)))?
        {
            let (name, id) = row?;
            if access.allows(crate::access::Action::Read, &name) {
                marks.insert(name, id);
            }
        }
        tx.commit()?;
        Ok(marks)
    }

    /// Record only positions the reader actually observed, not the head at click time.
    pub fn mark_channels_read(&self, actor: &str, positions: &[(&str, i64)]) -> Result<()> {
        if positions.is_empty() {
            return Ok(());
        }
        self.write(|tx| {
            let access = self.access_on(tx)?;
            let key = access.private_key(actor)?;
            let mut save = tx.prepare(
                "INSERT INTO channel_seen(actor,channel,message)
                SELECT ?1,?2,?3 WHERE EXISTS(SELECT 1 FROM channels WHERE name=?2)
                AND (?3=0 OR EXISTS(SELECT 1 FROM messages WHERE id=?3 AND channel=?2))
                ON CONFLICT(actor,channel) DO UPDATE SET message=max(message,excluded.message)",
            )?;
            for &(channel, id) in positions {
                access.require(crate::access::Action::Read, channel)?;
                ensure!(
                    save.execute(params![key, channel, id])? != 0,
                    "invalid channel read position"
                );
            }
            Ok(())
        })
    }

    pub fn channel(&self, name: &str) -> Result<Channel> {
        let tx = self.conn.unchecked_transaction()?;
        let a = self.access_on(&tx)?;
        ensure!(
            a.allows(crate::access::Action::Read, name),
            crate::errors::Kind::Missing
                .error("channel_not_found", format!("channel not found: {name}"))
        );
        let channel = tx
            .query_row(
                &format!("{COUNTS} WHERE c.name=? GROUP BY c.name"),
                [name],
                channel,
            )
            .optional()?
            .with_context(|| {
                crate::errors::Kind::Missing
                    .error("channel_not_found", format!("channel not found: {name}"))
            })?;
        tx.commit()?;
        Ok(channel)
    }

    pub fn channel_page(
        &self,
        after: &str,
        scope: Option<&str>,
        limit: usize,
    ) -> Result<(i64, Vec<Channel>, Option<String>)> {
        self.channels_window(after, scope, None, limit)
    }
    pub fn channels_window(
        &self,
        after: &str,
        scope: Option<&str>,
        prefix: Option<&str>,
        limit: usize,
    ) -> Result<(i64, Vec<Channel>, Option<String>)> {
        ensure!(
            scope.is_none() || prefix.is_none(),
            "use scope or prefix, not both"
        );
        ensure!(
            prefix.is_none_or(|s| !s.is_empty()
                && s.len() <= 501
                && !s.chars().any(char::is_control)),
            "invalid channel prefix"
        );
        ensure!((1..=500).contains(&limit), "limit must be 1..500");
        let root = scope.map(|s| s.strip_suffix('/').unwrap_or(s));
        if let Some(root) = root {
            validate_channel(root)?;
        }
        let tx = self.conn.unchecked_transaction()?;
        let a = self.access_on(&tx)?;
        let channels = a
            .channels(&tx, crate::access::Action::Read)?
            .map(|v| serde_json::to_string(&v))
            .transpose()?;
        let head = crate::read::cursor_on(&tx)?;
        let sql = format!(
            "{COUNTS} WHERE c.name > ?1 AND (?2 IS NULL OR c.name=?2 OR instr(c.name,?2||'/')=1)
            AND (?4 IS NULL OR c.name IN (SELECT value FROM json_each(?4))) AND (?5 IS NULL OR instr(c.name,?5)=1)
            GROUP BY c.name ORDER BY c.name LIMIT ?3"
        );
        let mut rows: Vec<_> = tx
            .prepare(&sql)?
            .query_map(params![after, root, limit + 1, channels, prefix], channel)?
            .collect::<rusqlite::Result<_>>()?;
        let next = (rows.len() > limit).then(|| rows[limit - 1].name.clone());
        rows.truncate(limit);
        tx.commit()?;
        Ok((head, rows, next))
    }
}
