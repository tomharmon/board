-- Only join a referenced task if it is actually in this run's readable channel.
, run_tasks AS (
    SELECT t.* FROM tasks t JOIN messages m USING(id) WHERE m.channel=?2 AND NOT m.deleted
), actions(cursor,kind,assigned_to,summary,source,task,worker) AS (
    SELECT coalesce((SELECT max(cursor) FROM events WHERE message_id=t.id AND kind='task.'||t.state),t.id),
        t.state,coalesce(t.assignee,substr(t.owner,1,instr(t.owner||'/','/')-1)),coalesce(t.result,t.state),t.id,t.id,NULL
    FROM run_tasks t WHERE t.state IN ('failed','blocked')
    UNION ALL
    SELECT cursor,kind,assigned_to,summary,source,task,worker FROM current_run_actions
)
SELECT a.cursor,a.kind,a.assigned_to,substr(a.summary,1,400),a.source,t.id,
    (SELECT count(*) FROM task_needs n JOIN run_tasks d ON d.id=n.task WHERE n.need=t.id AND t.state!='completed' AND d.state='waiting'),a.worker
FROM actions a LEFT JOIN run_tasks t ON a.task=t.id
WHERE a.cursor>?3 ORDER BY a.cursor LIMIT ?4
