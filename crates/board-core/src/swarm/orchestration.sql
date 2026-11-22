CREATE TABLE IF NOT EXISTS swarm_policy (
    run_id INTEGER PRIMARY KEY REFERENCES swarm_runs(id),
    max_active INTEGER NOT NULL CHECK(max_active BETWEEN 1 AND 128),
    retry_budget INTEGER NOT NULL CHECK(retry_budget BETWEEN 0 AND 1000)
);
CREATE INDEX IF NOT EXISTS swarm_latest_handoff ON swarm_evidence(run_id,actor,json_extract(payload,'$.task'),message_id) WHERE kind='handoff';
CREATE INDEX IF NOT EXISTS swarm_latest_check ON swarm_evidence(run_id,json_extract(payload,'$.handoff'),message_id) WHERE kind='verification';
CREATE INDEX IF NOT EXISTS swarm_resolution ON swarm_evidence(run_id,json_extract(payload,'$.request')) WHERE kind='decision-resolution';
CREATE INDEX IF NOT EXISTS swarm_task_handoffs ON swarm_evidence(run_id,json_extract(payload,'$.task'),message_id) WHERE kind='handoff';
CREATE TABLE IF NOT EXISTS swarm_public_seen (
    run_id INTEGER NOT NULL REFERENCES swarm_runs(id), viewer TEXT NOT NULL, through INTEGER NOT NULL,
    PRIMARY KEY(run_id,viewer)
);
CREATE TABLE IF NOT EXISTS swarm_worker_runtime (
    worker_id INTEGER PRIMARY KEY REFERENCES swarm_workers(id) ON DELETE CASCADE,
    model TEXT, workspace_mode TEXT, session_ref TEXT
);
