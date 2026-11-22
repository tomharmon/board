CREATE TABLE IF NOT EXISTS swarm_policy (
    run_id INTEGER PRIMARY KEY REFERENCES swarm_runs(id),
    max_active INTEGER NOT NULL CHECK(max_active BETWEEN 1 AND 128),
    retry_budget INTEGER NOT NULL CHECK(retry_budget BETWEEN 0 AND 1000)
);
CREATE TABLE IF NOT EXISTS swarm_plans (
    run_id INTEGER NOT NULL REFERENCES swarm_runs(id), key TEXT NOT NULL,
    actor TEXT NOT NULL, digest BLOB NOT NULL, PRIMARY KEY (run_id,key)
);
CREATE INDEX IF NOT EXISTS swarm_latest_handoff ON swarm_evidence(run_id,actor,json_extract(payload,'$.task'),message_id) WHERE kind='handoff';
CREATE INDEX IF NOT EXISTS swarm_latest_check ON swarm_evidence(run_id,json_extract(payload,'$.handoff'),message_id) WHERE kind='verification';
CREATE INDEX IF NOT EXISTS swarm_resolution ON swarm_evidence(run_id,json_extract(payload,'$.request')) WHERE kind='decision-resolution';
