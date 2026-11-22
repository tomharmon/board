-- Both readers supply the set of runs they may inspect in selected_runs.
, ranked_handoffs AS (
    SELECT h.*, row_number() OVER (PARTITION BY run_id,actor,thread_id ORDER BY message_id DESC) AS revision_rank
    FROM readable_evidence h WHERE kind='handoff'
), current_handoffs AS (
    SELECT h.*,
        (SELECT max(v.message_id) FROM readable_evidence v WHERE v.run_id=h.run_id
         AND v.kind='verification' AND json_extract(v.payload,'$.handoff')=h.message_id) AS verification
    FROM ranked_handoffs h WHERE revision_rank=1
), current_run_actions(run_id,cursor,kind,assigned_to,summary,source,thread_id,worker,waiting) AS (
    SELECT r.id,d.message_id,'decision',json_extract(d.payload,'$.to'),json_extract(d.payload,'$.summary'),
        d.message_id,coalesce(h.thread_id,d.thread_id),NULL,d.actor
    FROM selected_runs r JOIN readable_evidence d ON d.run_id=r.id AND d.kind='decision-request'
    LEFT JOIN readable_evidence h ON h.run_id=d.run_id
        AND h.message_id=json_extract(d.payload,'$.handoff') AND h.kind='handoff'
    WHERE NOT EXISTS (SELECT 1 FROM readable_evidence a WHERE a.run_id=d.run_id AND a.kind='decision-resolution'
        AND json_extract(a.payload,'$.request')=d.message_id)
    UNION ALL
    SELECT h.run_id,coalesce(v.message_id,h.message_id),CASE WHEN v.message_id IS NULL THEN 'awaiting_verification' ELSE 'verification' END,
        CASE WHEN v.message_id IS NULL THEN w.actor ELSE h.actor END,
        json_extract(coalesce(v.payload,h.payload),'$.summary'),coalesce(v.message_id,h.message_id),h.thread_id,NULL,coalesce(v.actor,h.actor)
    FROM current_handoffs h LEFT JOIN readable_evidence v ON v.message_id=h.verification
    JOIN swarm_workers w ON w.run_id=h.run_id AND w.name=json_extract(h.payload,'$.to')
    WHERE v.message_id IS NULL OR EXISTS
        (SELECT 1 FROM json_each(v.payload,'$.checks') WHERE json_extract(value,'$.status')!='pass')
    UNION ALL
    SELECT r.id,(SELECT max(e.cursor) FROM events e WHERE e.message_id=r.root_id AND e.kind='swarm.launch_failed'
        AND json_extract(e.payload,'$.worker')=w.name AND json_extract(e.payload,'$.run')=r.slug),'launch_failed',r.coordinator_actor,coalesce(w.launch_error,''),r.root_id,r.root_id,w.name,w.actor
    FROM selected_runs r JOIN swarm_workers w ON w.run_id=r.id
    WHERE r.status='active' AND w.status='launch_failed'
)
