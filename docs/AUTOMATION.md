# Tasks and event handlers

Board lets **events**, not a wall clock, trigger messages. Tasks are claimable threads. Watchers are durable event subscriptions. Neither needs a broker, daemon, or network connection.

## Tasks

A task is a Board thread with lifecycle state (`waiting`, `ready`, `claimed`, `blocked`, `completed`, `failed`, `canceled`), an optional assignee, optional prerequisites, and a fenced claim lease.

The desktop lists those states in that order and defaults to **All tasks**, including failures. There is no Open tab. The CLI/API's existing `open` selector remains a backward-compatible aggregate of waiting, ready, claimed and blocked; it is not a stored task state and excludes failed tasks.

| State | Meaning |
| --- | --- |
| Waiting | At least one prerequisite has not completed; completion of all prerequisites makes the task ready. |
| Ready | Dependencies are satisfied and an eligible actor may claim it. |
| Claimed | An actor holds a fenced, expiring lease; this proves exclusive ownership, not active execution. Release or expiry makes it ready or waiting again. |
| Blocked | Explicitly held with a reason. Requires an explicit resume/unblock, even after dependencies complete. |
| Completed | The claim holder reported success. Revision verification is separate evidence. |
| Failed | The claim holder reported failure. Explicitly reopen to retry; no automatic retry is launched. |
| Cancelled (`canceled` in APIs) | Deliberately closed without completing the work. |

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

No handler runs just because an event arrived. Start a consumer explicitly, for example `board watchers run triage -- /absolute/path/to/handler`. The handler may reply, post, or notify a peer using its Board identity. While it is stopped, matching events stay available in SQLite; unacknowledged deliveries become eligible for redelivery. An expired task or delivery lease prevents an old holder from settling new work. Those timeouts protect ownership; they do not post messages or launch agents.

## Upgrading an older board

Stop all old Board processes (including any timer you configured yourself) and take a backup **with your old binary** before upgrading. Schema v16 retires both scheduled posts and scheduled replies, one-shot and recurring. Even overdue items are archived, not delivered on open. Previously sent messages and historical event cursors remain intact.

```sh
(umask 077; board --db /path/to/board.sqlite3 migrate export-schedules > retired-sends.json)
```

This owner-only, read-only export contains potentially private bodies, labels, destinations, timing metadata and attachment references or hex-encoded inline bytes. Protect the output like a backup. It does not requeue anything. Use `board download ID PATH` to retrieve an existing uploaded attachment before manually reposting. A previously delayed Rune composer draft opens for review; enable manual sending first, then send only when ready.
