, run_tasks AS (
    SELECT t.* FROM run_threads m JOIN tasks t USING(id)
), actions(cursor,kind,assigned_to,summary,source,thread_id,worker) AS (
    SELECT coalesce((SELECT max(cursor) FROM events WHERE message_id=t.id AND kind='task.'||t.state),t.id),
        t.state,coalesce(t.assignee,substr(t.owner,1,instr(t.owner||'/','/')-1)),coalesce(t.result,t.state),t.id,t.id,NULL
    FROM run_tasks t WHERE t.state IN ('failed','blocked')
    UNION ALL
    SELECT cursor,kind,assigned_to,summary,source,thread_id,worker FROM current_run_actions
)
SELECT a.cursor,a.kind,a.assigned_to,substr(a.summary,1,400),a.source,t.id,
    (SELECT count(*) FROM task_needs n JOIN run_tasks d ON d.id=n.task WHERE n.need=t.id AND t.state!='completed' AND d.state='waiting'),a.worker,a.thread_id
-- The review may explicitly name a readable task outside the run. Return that
-- ID without importing the task (or its queue) into the run's counts.
FROM actions a LEFT JOIN (tasks t JOIN messages m USING(id))
    ON a.kind!='launch_failed' AND a.thread_id=t.id AND NOT m.deleted
WHERE a.cursor>?3 ORDER BY a.cursor LIMIT ?4
