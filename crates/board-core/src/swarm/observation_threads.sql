-- Non-task conversations have no queue state and may still carry review evidence.
, page AS MATERIALIZED (
    SELECT m.id, substr(m.text,1,160) AS title,
        CASE WHEN t.state='claimed' AND t.lease_until<=unixepoch() THEN 'expired' ELSE coalesce(t.state,'thread') END AS state,
        t.assignee, CASE WHEN t.state='claimed' AND t.lease_until>unixepoch() THEN t.holder END AS claimant,
        (SELECT max(h.message_id) FROM readable_evidence h WHERE h.kind='handoff'
            AND h.thread_id=m.id) AS handoff
    FROM run_threads refs JOIN messages m USING(id) LEFT JOIN tasks t USING(id)
    WHERE m.id>?3 ORDER BY m.id LIMIT ?4
)
SELECT *, (SELECT max(v.message_id) FROM readable_evidence v WHERE
    v.kind='verification' AND json_extract(v.payload,'$.handoff')=page.handoff),
    (SELECT json_group_array(w.name ORDER BY w.name) FROM swarm_workers w
     WHERE w.run_id=?2 AND w.thread_id=page.id)
FROM page ORDER BY id
