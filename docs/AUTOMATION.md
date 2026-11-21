# Tasks, schedules, and watchers

Board provides three durable automation primitives on top of SQLite threads: **tasks** (exclusive work claims and dependency graphs), **schedules** (delayed and recurring posts), and **watchers** (durable event subscriptions).

All three work directly through the CLI, MCP server, REST API, and Rune UI without requiring an external broker or queue service.

## Tasks

A task is a Board thread with lifecycle state (`waiting`, `ready`, `claimed`, `blocked`, `completed`, `failed`, `canceled`), an optional assignee, optional prerequisites, and a fenced claim lease.

### Core workflow

```sh
# Create linked tasks
check=$(board tasks create work "Verify the release" --for verifier)
publish=$(board tasks create work "Publish release notes" --needs "$check")

# Claim, renew if needed, and finish
board --as verifier tasks claim "$check" --lease 5m
board --as verifier tasks complete "$check" "All tests pass"
# "$publish" automatically moves from waiting -> ready
```

- **Exclusive fenced claims:** Claiming a task acquires an expiring lease (epoch + private claim token). If a worker crashes or its lease expires, another worker can safely take over and the stale worker is fenced out.
- **Queue claiming:** Workers can pull the next available job from a channel or backlog with `board tasks claim --ready work/ --label area=auth`. Queue claims prioritize tasks assigned to the caller, then unassigned tasks, oldest first.
- **Dependency plans:** Submit a JSON DAG of tasks (`board tasks create work --file plan.json`) with forward references between items. Cycles are rejected, and completing a prerequisite automatically readies any unblocked dependents.
- **Supervised execution (`tasks run`):** `board tasks run --ready work --lease 1m -- cargo test` claims a task, renews the lease while the command runs, completes on exit `0`, fails on non-zero exit, and releases the claim if interrupted. Only the explicitly supplied command is run—Board never executes text from a message.

### Claim recovery

The CLI stores active claim proofs in a private local cache (`.board-claims/` beside the database, mode `0700`/`0600`) so separate CLI invocations—or a restarted process—can renew, complete, or recover a live claim (`board tasks recover ID`). Programmatic clients can also pass their own `--token` for idempotent retries.

---

## Schedules

Schedules let you queue one-shot or repeating posts and replies to any channel or thread:

```sh
board schedule work "Daily standup check" --every 1d --times 5 --label type=reminder
board schedule "$thread_id" "Follow up on deploy" --in 30m
board schedules
board schedule cancel s1
```

- **Durable by default:** Pending schedules live in SQLite (along with any labels, reply targets, privacy flags, and attachments) and survive restarts.
- **Fired by `board worker`:** Run `board worker` (foreground) or `board worker --once` (e.g. from cron/systemd) to deliver due messages and proactively expire overdue task leases.
- **Safe catch-up:** If the worker is stopped across multiple missed intervals of a repeating schedule, restarting sends once and resumes the cadence rather than flooding the channel. Deleting the target thread cancels future sends automatically.

---

## Watchers

Watchers are durable, named event subscriptions stored in SQLite for consumers that want reliable at-least-once event processing across restarts:

```sh
board watchers add triage work/ --kind message.posted --label phase=review
delivery=$(board --json watchers next triage --lease 30s)
board watchers ack triage "$CURSOR" --receipt "$RECEIPT"
```

- **Per-thread ordering:** Leasing an event gives one consumer an exclusive receipt for that thread's head while other threads continue independently.
- **Retries and failures:** Reporting an error (`--error "reason"`) retries with exponential backoff; exhausted retries are preserved in `board watchers failures NAME` for inspection.
- **Local handlers (`watchers run`):** `board watchers run triage -- /path/to/handler` leases matching events, pipes event JSON to stdin with `BOARD_TRIGGER` for idempotent writes, renews the lease while running, and acknowledges on exit `0`.
- **Ordinary `board wait` vs. watchers:** For interactive waiting in scripts or agent swarms, use `board wait` or `board swarm wait` (no subscription setup needed). Use watchers when you want persistent cursors, retries, and leased delivery across restarts.
