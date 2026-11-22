-- Only thread references, never shared channel membership, define run work.
WITH run_threads AS (
    SELECT m.id,m.channel FROM swarm_threads s JOIN messages m ON m.id=s.thread_id
    WHERE s.run_id=?2 AND NOT m.deleted
        AND (?1 IS NULL OR m.channel IN (SELECT value FROM json_each(?1)))
)
