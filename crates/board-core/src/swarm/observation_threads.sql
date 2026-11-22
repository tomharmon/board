-- Page ordinary task roots before looking up their latest revision-bound evidence.
WITH page AS MATERIALIZED (
    SELECT m.id, substr(m.text,1,160) AS title,
        coalesce(w.value,p.value) AS workstream,
        CASE WHEN t.state='claimed' AND t.lease_until<=unixepoch() THEN 'expired' ELSE t.state END AS state,
        t.assignee, CASE WHEN t.state='claimed' AND t.lease_until>unixepoch() THEN t.holder END AS claimant,
        (SELECT max(h.message_id) FROM swarm_evidence h WHERE h.run_id=?2 AND h.kind='handoff'
            AND json_extract(h.payload,'$.task')=m.id) AS handoff
    FROM messages m JOIN tasks t USING(id)
    LEFT JOIN labels w ON w.message_id=m.id AND w.key='workstream'
    LEFT JOIN labels p ON p.message_id=m.id AND p.key='pool'
    WHERE m.channel=?1 AND m.id>?3 AND NOT m.deleted ORDER BY m.id LIMIT ?4
)
SELECT *, (SELECT max(v.message_id) FROM swarm_evidence v WHERE v.run_id=?2
    AND v.kind='verification' AND json_extract(v.payload,'$.handoff')=page.handoff)
FROM page ORDER BY id
