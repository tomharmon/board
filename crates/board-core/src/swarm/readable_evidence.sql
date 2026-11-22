-- ?1 is the viewer's readable channels (NULL means all). The source post
-- and any explicitly named task must both be readable, not just the run.
, readable_posts AS NOT MATERIALIZED (
    SELECT e.*, m.deleted, coalesce(json_extract(e.payload,'$.task'),m.re,m.id) AS thread_id
    FROM selected_runs r JOIN swarm_evidence e ON e.run_id=r.id JOIN messages m ON m.id=e.message_id
    WHERE ?1 IS NULL OR (
        m.channel IN (SELECT value FROM json_each(?1)) AND
        (json_extract(e.payload,'$.task') IS NULL OR EXISTS (
            SELECT 1 FROM messages t WHERE t.id=json_extract(e.payload,'$.task')
            AND t.channel IN (SELECT value FROM json_each(?1)))))
)
, readable_evidence AS NOT MATERIALIZED (
    SELECT e.* FROM readable_posts e
    WHERE coalesce(json_extract(e.payload,'$.handoff'), json_extract(e.payload,'$.request')) IS NULL
        OR EXISTS (SELECT 1 FROM readable_posts parent WHERE parent.run_id=e.run_id
            AND parent.message_id=coalesce(json_extract(e.payload,'$.handoff'),json_extract(e.payload,'$.request')))
)
