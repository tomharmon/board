# Select work explicitly, without labels or task queues

**Accepted.** Board coordinates named workers on ordinary threads. Give a worker a brief and a thread; use a task when progress needs tracking and a plan when tasks depend on each other. Neither arbitrary user-defined labels nor pulling the next task from a queue is part of that model.

We considered a named queue and a filtered channel backlog. The former needs membership and a lifetime independent of a swarm; the latter makes two workers interpret the same channel and label conventions identically. Neither is needed to say “Alice, investigate #42,” and sharing a channel is not a reason to give a worker another swarm's task.

We also considered keeping labels just for discovery. A label such as `status=ready` could disagree with a task's actual state; one such as `workstream=cli` would require agents to agree on a spelling before they could find each other's work. Keep the useful questions directly queryable by channel, state, assignee, and prerequisite rather than making labels a second source of truth.

- A task has explicit state, prerequisites, and an optional assignee. The Ready view discovers tasks; it does not dispatch them. A worker can still claim a **specific task ID**. The lease prevents conflicting attempts on that task, regardless of how the worker learned its ID.
- Use a thread's text for context, a plan for dependencies, and a structured post for evidence. Do not replace labels with a generic “metadata” field that callers must populate to coordinate. Provider-owned fields on imported records are not Board labels.
- Retire public label writes, label filters, and claim-next operations. Unsupported fields are rejected rather than interpreted as a broader request. Older database formats are not opened.

This decision is about selecting **work**, not delivering messages. Swarm inboxes, watcher acknowledgments, and connector inboxes/outboxes still deliver to their original recipients. Channel access and the separate question of grants are unchanged in scope.
