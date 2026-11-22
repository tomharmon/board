CREATE INDEX IF NOT EXISTS swarm_runs_by_root ON swarm_runs(root_id);
CREATE INDEX IF NOT EXISTS messages_by_thread ON messages(coalesce(re,id),id);
CREATE INDEX IF NOT EXISTS swarm_workers_by_thread ON swarm_workers(thread_id,run_id) WHERE thread_id IS NOT NULL;
CREATE TABLE IF NOT EXISTS swarm_plan_tasks(
    run_id INTEGER NOT NULL REFERENCES swarm_runs(id), thread_id INTEGER NOT NULL REFERENCES tasks(id),
    PRIMARY KEY(run_id,thread_id)
);
CREATE INDEX IF NOT EXISTS swarm_plan_tasks_by_thread ON swarm_plan_tasks(thread_id,run_id);
CREATE VIEW IF NOT EXISTS swarm_threads(run_id,thread_id) AS
    SELECT id,root_id FROM swarm_runs
    UNION SELECT run_id,thread_id FROM swarm_workers WHERE thread_id IS NOT NULL
    UNION SELECT run_id,thread_id FROM swarm_plan_tasks;
