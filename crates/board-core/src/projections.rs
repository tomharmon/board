use anyhow::Result;
use rusqlite::Connection;

/// Message IDs are posting cursors. A channel counts all posts, including tombstones,
/// but a thread's latest activity and reply count include only its surviving replies.
pub(super) fn init(c: &Connection) -> Result<()> {
    c.execute_batch("
        CREATE TABLE IF NOT EXISTS channel_activity(
            channel TEXT PRIMARY KEY REFERENCES channels(name), roots INTEGER NOT NULL,
            total INTEGER NOT NULL, latest INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS thread_activity(
            root INTEGER PRIMARY KEY REFERENCES messages(id), channel TEXT NOT NULL,
            replies INTEGER NOT NULL, activity INTEGER NOT NULL);
        CREATE UNIQUE INDEX IF NOT EXISTS thread_activity_all ON thread_activity(activity);
        CREATE INDEX IF NOT EXISTS events_by_actor ON events(actor);
        CREATE INDEX IF NOT EXISTS thread_activity_channel ON thread_activity(channel,activity);
        CREATE INDEX IF NOT EXISTS live_replies_by_root ON messages(re,id) WHERE re IS NOT NULL AND deleted=0;
        CREATE TRIGGER IF NOT EXISTS activity_on_post AFTER INSERT ON messages BEGIN
            INSERT INTO channel_activity VALUES(new.channel,new.re IS NULL,1,new.id)
              ON CONFLICT(channel) DO UPDATE SET total=total+1,roots=roots+excluded.roots,latest=max(latest,excluded.latest);
            INSERT INTO thread_activity SELECT new.id,new.channel,0,new.id WHERE new.re IS NULL;
            UPDATE thread_activity SET replies=replies+1,activity=max(activity,new.id) WHERE root=new.re AND new.deleted=0;
        END;
        CREATE TRIGGER IF NOT EXISTS activity_on_tombstone AFTER UPDATE OF deleted ON messages
        WHEN new.re IS NOT NULL AND new.deleted!=old.deleted BEGIN
            UPDATE thread_activity SET replies=replies+(CASE WHEN new.deleted=0 THEN 1 ELSE -1 END),
                activity=CASE WHEN new.deleted=0 THEN max(activity,new.id) WHEN activity=new.id
                THEN coalesce((SELECT max(id) FROM messages WHERE re=new.re AND deleted=0),root) ELSE activity END
            WHERE root=new.re;
        END;")?;
    Ok(())
}
