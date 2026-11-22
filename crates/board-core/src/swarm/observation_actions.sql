-- Only join a referenced task if it is actually in this run's readable channel.
WITH run_tasks AS (
    SELECT t.* FROM tasks t JOIN messages m USING(id) WHERE m.channel=?2 AND NOT m.deleted
), handoffs AS (
    SELECT h.*, json_extract(h.payload,'$.task') AS task,
        (SELECT max(v.message_id) FROM swarm_evidence v WHERE v.run_id=h.run_id
         AND v.kind='verification' AND json_extract(v.payload,'$.handoff')=h.message_id) AS verification
    FROM swarm_evidence h WHERE h.run_id=?1 AND h.kind='handoff'
      AND NOT EXISTS (SELECT 1 FROM swarm_evidence n WHERE n.run_id=h.run_id AND n.kind='handoff'
        AND n.actor=h.actor AND json_extract(n.payload,'$.task') IS json_extract(h.payload,'$.task')
        AND n.message_id>h.message_id)
), actions(cursor,kind,assigned_to,summary,source,task,worker) AS (
    SELECT coalesce((SELECT max(cursor) FROM events WHERE message_id=t.id AND kind='task.'||t.state),t.id),
        t.state,coalesce(t.assignee,substr(t.owner,1,instr(t.owner||'/','/')-1)),coalesce(t.result,t.state),t.id,t.id,NULL
    FROM run_tasks t WHERE t.state IN ('failed','blocked')
    UNION ALL
    SELECT d.message_id,'decision',json_extract(d.payload,'$.to'),json_extract(d.payload,'$.summary'),
        d.message_id,json_extract(h.payload,'$.task'),NULL
    FROM swarm_evidence d LEFT JOIN swarm_evidence h ON h.run_id=d.run_id
        AND h.message_id=json_extract(d.payload,'$.handoff') AND h.kind='handoff'
    WHERE d.run_id=?1 AND d.kind='decision-request' AND NOT EXISTS
        (SELECT 1 FROM swarm_evidence a WHERE a.run_id=d.run_id AND a.kind='decision-resolution'
         AND json_extract(a.payload,'$.request')=d.message_id)
    UNION ALL
    SELECT coalesce(v.message_id,h.message_id),CASE WHEN v.message_id IS NULL THEN 'awaiting_verification' ELSE 'verification' END,
        CASE WHEN v.message_id IS NULL THEN w.actor ELSE h.actor END,
        json_extract(coalesce(v.payload,h.payload),'$.summary'),coalesce(v.message_id,h.message_id),h.task,NULL
    FROM handoffs h LEFT JOIN swarm_evidence v ON v.message_id=h.verification
    JOIN swarm_workers w ON w.run_id=h.run_id AND w.name=json_extract(h.payload,'$.to')
    WHERE v.message_id IS NULL OR EXISTS
        (SELECT 1 FROM json_each(v.payload,'$.checks') WHERE json_extract(value,'$.status')!='pass')
    UNION ALL
    SELECT (SELECT max(e.cursor) FROM events e WHERE e.message_id=?4 AND e.kind='swarm.launch_failed'
        AND json_extract(e.payload,'$.worker')=w.name),'launch_failed',?3,w.launch_error,?4,NULL,w.name
    FROM swarm_workers w JOIN swarm_runs r ON r.id=w.run_id
    WHERE r.id=?1 AND r.status='active' AND w.status='launch_failed'
)
SELECT a.cursor,a.kind,a.assigned_to,substr(a.summary,1,400),a.source,t.id,
    (SELECT count(*) FROM task_needs n JOIN run_tasks d ON d.id=n.task WHERE n.need=t.id AND t.state!='completed' AND d.state='waiting'),a.worker
FROM actions a LEFT JOIN run_tasks t ON a.task=t.id
WHERE a.cursor>?5 ORDER BY a.cursor LIMIT ?6
