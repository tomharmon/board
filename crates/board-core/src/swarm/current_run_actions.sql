-- Both readers supply the set of runs they may inspect in selected_runs.
, current_handoffs AS (
    SELECT h.*, json_extract(h.payload,'$.task') AS task,
        (SELECT max(v.message_id) FROM swarm_evidence v WHERE v.run_id=h.run_id
         AND v.kind='verification' AND json_extract(v.payload,'$.handoff')=h.message_id) AS verification
    FROM selected_runs r JOIN swarm_evidence h ON h.run_id=r.id AND h.kind='handoff'
    WHERE NOT EXISTS (SELECT 1 FROM swarm_evidence n WHERE n.run_id=h.run_id AND n.kind='handoff'
        AND n.actor=h.actor AND json_extract(n.payload,'$.task') IS json_extract(h.payload,'$.task')
        AND n.message_id>h.message_id)
), current_run_actions(run_id,cursor,kind,assigned_to,summary,source,task,worker,waiting) AS (
    SELECT r.id,d.message_id,'decision',json_extract(d.payload,'$.to'),json_extract(d.payload,'$.summary'),
        d.message_id,json_extract(h.payload,'$.task'),NULL,d.actor
    FROM selected_runs r JOIN swarm_evidence d ON d.run_id=r.id AND d.kind='decision-request'
    LEFT JOIN swarm_evidence h ON h.run_id=d.run_id
        AND h.message_id=json_extract(d.payload,'$.handoff') AND h.kind='handoff'
    WHERE NOT EXISTS (SELECT 1 FROM swarm_evidence a WHERE a.run_id=d.run_id AND a.kind='decision-resolution'
        AND json_extract(a.payload,'$.request')=d.message_id)
    UNION ALL
    SELECT h.run_id,coalesce(v.message_id,h.message_id),CASE WHEN v.message_id IS NULL THEN 'awaiting_verification' ELSE 'verification' END,
        CASE WHEN v.message_id IS NULL THEN w.actor ELSE h.actor END,
        json_extract(coalesce(v.payload,h.payload),'$.summary'),coalesce(v.message_id,h.message_id),h.task,NULL,coalesce(v.actor,h.actor)
    FROM current_handoffs h LEFT JOIN swarm_evidence v ON v.message_id=h.verification
    JOIN swarm_workers w ON w.run_id=h.run_id AND w.name=json_extract(h.payload,'$.to')
    WHERE v.message_id IS NULL OR EXISTS
        (SELECT 1 FROM json_each(v.payload,'$.checks') WHERE json_extract(value,'$.status')!='pass')
    UNION ALL
    SELECT r.id,(SELECT max(e.cursor) FROM events e WHERE e.message_id=r.root_id AND e.kind='swarm.launch_failed'
        AND json_extract(e.payload,'$.worker')=w.name),'launch_failed',r.coordinator_actor,coalesce(w.launch_error,''),r.root_id,NULL,w.name,w.actor
    FROM selected_runs r JOIN swarm_workers w ON w.run_id=r.id
    WHERE r.status='active' AND w.status='launch_failed'
)
