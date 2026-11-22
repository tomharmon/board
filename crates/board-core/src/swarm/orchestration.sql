CREATE INDEX IF NOT EXISTS swarm_latest_handoff ON swarm_evidence(run_id,actor,json_extract(payload,'$.task'),message_id) WHERE kind='handoff';
CREATE INDEX IF NOT EXISTS swarm_latest_check ON swarm_evidence(run_id,json_extract(payload,'$.handoff'),message_id) WHERE kind='verification';
CREATE INDEX IF NOT EXISTS swarm_resolution ON swarm_evidence(run_id,json_extract(payload,'$.request')) WHERE kind='decision-resolution';
CREATE INDEX IF NOT EXISTS swarm_task_handoffs ON swarm_evidence(run_id,json_extract(payload,'$.task'),message_id) WHERE kind='handoff';
CREATE TABLE IF NOT EXISTS swarm_public_seen (
    run_id INTEGER NOT NULL REFERENCES swarm_runs(id), viewer TEXT NOT NULL, through INTEGER NOT NULL,
    PRIMARY KEY(run_id,viewer)
);
