-- Start each enabled channel mirror at most once, in the same commit as the post.
-- No network is involved. A pending lane is renamed only after a confirmed receipt.
DROP TRIGGER IF EXISTS sync_mirror_root;
DROP TRIGGER IF EXISTS sync_render_labels;
DROP TRIGGER IF EXISTS sync_mirror_asset;
DROP TRIGGER IF EXISTS sync_remove_asset;
UPDATE sync_settings SET value=json_set(value,'$.mirror_roots',json('false'))
 WHERE json_extract(value,'$.mirror_roots')=1 AND source IN (SELECT name FROM sync_sources WHERE provider='github');
CREATE TRIGGER IF NOT EXISTS sync_mirror_root AFTER UPDATE OF message ON events
WHEN NEW.kind='message.posted' AND NEW.message IS NOT NULL
 AND json_extract(NEW.message,'$.re') IS NULL
 AND NOT EXISTS(SELECT 1 FROM private_messages WHERE id=NEW.message_id)
BEGIN
  INSERT OR IGNORE INTO sync_threads(source,thread,root)
    SELECT s.name,'pending:board:'||NEW.cursor,NEW.message_id
    FROM sync_sources s JOIN sync_settings p ON p.source=s.name
    WHERE s.outbound=1 AND s.provider!='github' AND s.channel=json_extract(NEW.message,'$.channel')
      AND json_extract(p.value,'$.mirror_roots')=1
      AND NOT EXISTS(SELECT 1 FROM sync_import_context WHERE source=s.name)
      AND NOT EXISTS(SELECT 1 FROM sync_starts WHERE source=s.name AND root=NEW.message_id);
  INSERT OR IGNORE INTO sync_queue(source,direction,thread,event_key,payload,raw)
    SELECT t.source,'out',t.thread,CAST(NEW.cursor AS TEXT),
      json_object('event','thread.start','message',NEW.message_id,'cursor',NEW.cursor,'text',json_extract(NEW.message,'$.text'),'parent',null),'null'
    FROM sync_threads t WHERE t.root=NEW.message_id AND t.thread='pending:board:'||NEW.cursor;
  INSERT OR IGNORE INTO sync_starts(source,root,item)
    SELECT q.source,NEW.message_id,q.id FROM sync_queue q WHERE q.direction='out'
      AND q.thread='pending:board:'||NEW.cursor AND q.event_key=CAST(NEW.cursor AS TEXT);
END;

-- Only renderers that use the changed values need to edit a confirmed text copy.
CREATE TRIGGER IF NOT EXISTS sync_render_labels AFTER UPDATE OF message ON events
WHEN NEW.message IS NOT NULL AND (NEW.kind='labels.changed' OR NEW.kind LIKE 'task.%')
 AND NOT EXISTS(SELECT 1 FROM private_messages WHERE id IN (NEW.message_id,json_extract(NEW.message,'$.re')))
BEGIN
 INSERT OR IGNORE INTO sync_queue(source,direction,thread,event_key,payload,raw)
 SELECT c.source,'out',c.thread,CAST(NEW.cursor AS TEXT),json_object('event','message.edited','message',c.message,'cursor',NEW.cursor,'text',json_extract(NEW.message,'$.text'),'parent',null),'null'
 FROM sync_copies c JOIN sync_sources s ON s.name=c.source JOIN sync_renderers r ON r.source=c.source
 WHERE c.message=NEW.message_id AND c.origin=0 AND s.outbound=1 AND NOT EXISTS(SELECT 1 FROM sync_import_context WHERE source=s.name);
END;

-- Files learned after a text receipt also fan out, never back onto their source.
CREATE TRIGGER IF NOT EXISTS sync_mirror_asset AFTER UPDATE OF message ON events
WHEN NEW.kind='attachment.added' AND NEW.message IS NOT NULL
 AND NOT EXISTS(SELECT 1 FROM private_messages WHERE id IN (NEW.message_id,json_extract(NEW.message,'$.re')))
BEGIN
 INSERT OR IGNORE INTO sync_queue(source,direction,thread,event_key,payload,raw)
 SELECT c.source,'out',c.thread,'file:'||c.message||':'||json_extract(NEW.payload,'$.id'),
 json_object('event','file.share','message',c.message,'cursor',NEW.cursor,'text','','parent',null,'asset',json_extract(NEW.payload,'$.id')),'null'
 FROM sync_copies c JOIN sync_sources s ON s.name=c.source JOIN sync_settings p ON p.source=c.source
 WHERE c.message=NEW.message_id AND c.origin=0 AND s.outbound=1 AND s.provider='slack' AND json_extract(p.value,'$.mirror_files')=1;
END;

CREATE TRIGGER IF NOT EXISTS sync_remove_asset AFTER UPDATE OF message ON events
WHEN NEW.kind='message.deleted' AND NEW.message IS NOT NULL
BEGIN
 INSERT OR IGNORE INTO sync_queue(source,direction,thread,event_key,payload,raw)
 SELECT c.source,'out',c.thread,'unfile:'||c.message||':'||c.asset,
 json_object('event','file.delete','message',c.message,'cursor',NEW.cursor,'text','','parent',null,'asset',c.asset),'null'
 FROM sync_file_copies c JOIN sync_sources s ON s.name=c.source
 WHERE c.message=NEW.message_id AND c.direction='out' AND s.outbound=1;
END;
