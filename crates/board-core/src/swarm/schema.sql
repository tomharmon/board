CREATE TABLE IF NOT EXISTS swarm_runs (
    id INTEGER PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE, goal TEXT NOT NULL,
    channel TEXT NOT NULL UNIQUE REFERENCES channels(name),
    root_id INTEGER NOT NULL REFERENCES messages(id), repo_root TEXT,
    coordinator_actor TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('active','completed','stopped')),
    max_notifications INTEGER NOT NULL, notifications_sent INTEGER NOT NULL DEFAULT 0,
    coordinator_cursor INTEGER NOT NULL DEFAULT 0, coordinator_pid INTEGER,
    created_at TEXT NOT NULL, ended_at TEXT, stop_cursor INTEGER
);
CREATE TABLE IF NOT EXISTS swarm_workers (
    id INTEGER PRIMARY KEY, run_id INTEGER NOT NULL REFERENCES swarm_runs(id) ON DELETE CASCADE,
    name TEXT NOT NULL, actor TEXT NOT NULL, role_slug TEXT, pinned_role_version INTEGER,
    pinned_system_prompt TEXT NOT NULL, pinned_skills_json TEXT NOT NULL,
    brief TEXT NOT NULL, workspace_path TEXT, branch_name TEXT,
    status TEXT NOT NULL CHECK (status IN ('registered','working','waiting','done','stopped','launch_failed')),
    waiter_pid INTEGER, last_read_cursor INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
    checked_in_at TEXT, last_activity_at TEXT, launch_error TEXT,
    UNIQUE(run_id,name), UNIQUE(run_id,actor)
);
CREATE INDEX IF NOT EXISTS swarm_workers_by_run ON swarm_workers(run_id,id);
CREATE TABLE IF NOT EXISTS swarm_dms (
    id INTEGER PRIMARY KEY, run_id INTEGER NOT NULL REFERENCES swarm_runs(id) ON DELETE CASCADE,
    from_actor TEXT NOT NULL, to_worker TEXT NOT NULL, text TEXT NOT NULL,
    event_cursor INTEGER NOT NULL UNIQUE REFERENCES events(cursor), created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS swarm_dms_by_run ON swarm_dms(run_id,event_cursor);
CREATE TABLE IF NOT EXISTS swarm_notifications (
    id INTEGER PRIMARY KEY, run_id INTEGER NOT NULL REFERENCES swarm_runs(id) ON DELETE CASCADE,
    target_worker TEXT NOT NULL, source_kind TEXT NOT NULL CHECK(source_kind IN ('mention','dm','system')),
    from_actor TEXT NOT NULL, message_id INTEGER REFERENCES messages(id), dm_id INTEGER REFERENCES swarm_dms(id),
    event_cursor INTEGER NOT NULL REFERENCES events(cursor), created_at TEXT NOT NULL,
    UNIQUE(run_id,target_worker,event_cursor)
);
CREATE INDEX IF NOT EXISTS swarm_notif_message ON swarm_notifications(message_id,target_worker);
CREATE TABLE IF NOT EXISTS swarm_evidence (
    message_id INTEGER PRIMARY KEY REFERENCES messages(id),
    run_id INTEGER NOT NULL REFERENCES swarm_runs(id),
    kind TEXT NOT NULL, actor TEXT NOT NULL, payload TEXT NOT NULL, created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS swarm_evidence_by_run ON swarm_evidence(run_id,message_id);
