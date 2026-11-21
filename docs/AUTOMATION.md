# Local work and automation

All commands below operate directly on the SQLite file selected by `--db` or
`BOARD_DB`. None needs the REST server. An HTTP client can use the same
resources on the optional authenticated loopback server; paths below start at
`/v1`. Use independent OS processes for parallel work.

## Task claims and recovery

```sh
export BOARD_DB=/tmp/board-demo.db BOARD_USER=ada
job=$(board tasks create work 'Verify the build' --for reviewer)
board --as reviewer tasks claim "$job" --lease 5m
board --as reviewer tasks renew "$job" --lease 5m
board --as reviewer tasks complete "$job" 'All checks passed'
board tasks work/ --state completed --limit 25 --after 0
board tasks work/ --latest --limit 10
```

Use `tasks create CHANNEL TEXT --claim` to create and claim in one SQLite
transaction, or `tasks create MESSAGE_ID --claim` to adopt and claim an
existing root. Unmet dependencies or an incompatible assignment roll the whole
write back. `--needs ID`, `--label KEY=VALUE`, `--attach FILE` and reusable
`--attachment-id ID_OR_KEY` are accepted on new task roots. Assign only a
profile of the task owner's person. Queue claims prefer work assigned to the
caller, then unassigned work, oldest first. An unrelated owner's pool work
requires `--from PERSON`; a direct ID is an explicit choice. Queue labels are
exact; list filters also support presence and alternatives. Lists accept
`--state`, `--needs`, `--by` (root author), `--owner`, `--for`,
`--label`, `--limit` and `--after`.
`--latest` selects the most recently active tasks before limiting, and displays
the window oldest to newest. It cannot be combined with `--after`. List JSON
includes the undeleted reply count and most recent live reply. The task's
`owner` is its person; `by` is the exact profile that created or adopted it.
The list filter `--by` still names the original root author. `--by me`
matches only the exact active actor, not its other profiles. An owner who
can read and post may change its task even without label permission. Taking
a claim requires label permission; a sibling profile is not its owner.

A task waits for at most 20 distinct prerequisites. Repeated IDs count once.
Changing the set checks only newly added edges, stopping after 500 nodes.
Completed, readable prerequisites can be pruned; hidden ones still count,
so rejection does not disclose whether hidden work is finished. Removing
edges does not retraverse unchanged history.

An intake credential may create task roots and plans without read access.
It gets its own immutable creation response on a keyed retry, not later
claims, results, edits, or discussion. Plan-internal named edges are allowed;
ordinary prerequisite IDs must name readable tasks. Creating and claiming
in one request needs read and label as well as post. Use `--no-sync` to keep
a new task or scheduled send from entering any outbound queue.

The CLI persists a random proof **before** claiming and stores the task ID
after the write commits. `tasks recover ID` checks live proofs, including
ones left pending if the process died between those writes. Separate CLI
processes can renew, release, complete, fail, or block by ID. The cache is a
separate SQLite file per canonical database path, board identity and exact
actor, under `.board-claims/` beside the database. Override the directory
with `BOARD_CLAIMS_DIR`. Unix directories must be owned by the current user
and private (0700); cache files are 0600. This directory contains secrets.
Backing up the board alone does not back up your local claim proofs.

For clients that keep their own secret, supply `--token TOKEN` on claim or
create-and-claim, or set `BOARD_REQUEST_TOKEN`. Use a fresh unpredictable
16-200 byte printable ASCII secret (no whitespace or double quotes) per
operation, never text from a task.
Passing a secret through the environment keeps it off the process argument
list. `tasks recover --token TOKEN` also works without the task ID. The board
stores only a digest and fences the proof to the actor, task, request and
epoch. Replaying a live keyed claim returns that same lease without extending
it or taking another task. Once released, completed, canceled or expired, the
old proof cannot reclaim. Ordinary reads and events omit tokens. Successful
claim, recovery and renewal responses include the holder's `claim_token`: do not
publish it. Explicit `--epoch` and `--token` on claim-bound writes continue
to work without relying on a cached proof.

```sh
board tasks create work 'Check the current checkout'
board tasks run --ready work --lease 1m --result 'Tests passed' -- cargo test --offline
```

`run` executes only the program and arguments supplied by the operator after
`--`. It does not evaluate board text, use an implicit shell, or choose a
program from a message. The child inherits the database and actor and receives
`BOARD_TASK`. The parent renews at a third of the requested lease; successful
exit completes, nonzero exit fails, and launch failure releases the task.
With `--json`, child stdout goes to stderr and stdout remains a single JSON
document. On Unix, Ctrl-C terminates the child's process group, releases the
claim and exits 130. Loss of a claim at renewal stops the group without
overwriting the new state. Polling cannot fence arbitrary side effects inside
an external command; make those effects independently idempotent. Windows
does not implement the Unix process-group or signal behavior.

`tasks block ID REASON --kind KIND` preserves a hold kind. The board reserves
`edited` for a task root changed by someone other than its owner. Reopening a
failed task and unblocking a held task reevaluate its dependencies.

## Task plans

```sh
board --json tasks create work --idempotency-key release42 --file - <<'JSON'
[
  {"key":"publish","text":"Publish","needs":[{"key":"test"}]},
  {"key":"test","text":"Run tests","labels":{"phase":"check"}}
]
JSON
```

A plan item accepts `text`, optional `key`, `labels`, `for`, `no_sync`,
`attachment_ids`, and `needs` (existing numeric IDs or `{"key":"..."}`
within the file). Up to 100 items commit as one transaction, including
forward references. Larger plans commit dependency-ordered chunks of at most
100, each with a durable checkpoint. Earlier chunks are real tasks that
independent processes can claim while later ones are pending. A failed
chunk leaves no partial tasks. Retry the same key to resume, including after
a restart. You may repair uncommitted items but cannot change a committed
item's original position, content or dependencies. A finished plan rejects
additional or changed items. Results and keyed lookups use original input
order and show current task states. An interrupted *multi-chunk* submission
without a user-provided key reports its generated key on ordinary errors;
supply a key in advance for recovery after a hard process kill.

## Schedules

```sh
board schedule work 'Daily check' --every 1d --times 5 --label type=reminder
board schedule "$job" --in 10m --file reminder.txt --attach evidence.log
board schedules "$job" --by ada --all
board worker --once
board worker
```

`worker` sends due local messages and proactively expires local task leases.
It does not run task commands, watcher callbacks or external sync dispatch.
Without it, pending schedules persist but no messages fire. Restarting
coalesces missed repeat slots into one send, retaining cadence and the
original actor. A send, its immutable attachments, its event, and advancement
of its ordinal commit together. Channel targets create new threads. A message
target retains the individual reply target as well as its thread root.
Deleting that target or root cancels future sends. Thread reads include
pending schedules; `schedules MESSAGE_ID` lists schedules anywhere in that
thread. `schedule cancel sID` is idempotent for an ended schedule.

Post-only credentials may schedule **new roots**, not replies to unreadable
threads. They may list, fetch and cancel their own schedules originally
created without read access, not other schedules under the same actor that
were made with read access. The worker rechecks the original credential,
current grants, any temporary request cap retained at creation, and access to
referenced uploads. If posting is no longer permitted the schedule becomes
`revoked` and nothing posts. UTC deadline fields in REST are Unix seconds;
CLI `--at` and `--until` take RFC 3339 timestamps. Intervals are whole
seconds, at least a minute for repeats. A new first send more than 60 seconds
in the past is rejected. That check is never applied to an already-persisted
deadline on recovery.

Mentions settle at registration, never again when dispatching the saved text.
An unresolved handle stays plain even if somebody appears under it later.
The creation response reports ambiguous or unresolved mentions. Schedules
report `first`, `next`, the last posted message ID, and cancellation details.
An older schedule without a surviving creation record may have no `first`.
A local write rule that drops a due send changes its state to `dropped`
without posting or blocking other schedules.
Scheduling a saved desktop draft commits its removal in the same transaction
as the schedule. A stale window cannot retire a changed draft; a failed
schedule write leaves the saved draft available when the board reopens.

## Watchers

```sh
board watchers add triage work/ --kind message.posted --label phase
board --json watchers next triage
board watchers ack triage CURSOR
board --json watchers next triage --lease 30s
board watchers ack triage CURSOR --receipt RECEIPT --skip
board watchers ack triage CURSOR --receipt RECEIPT --if-pending
board watchers ack triage CURSOR --receipt RECEIPT --error 'temporary failure'
board watchers failures triage
board watchers run triage --once -- /absolute/path/to/local-handler
board watchers replace subscriptions.json
board --json watchers matches --limit 10 --lease 5m
```

Subscriptions start at registration. The ordinary peek/ack pair is for a
single consumer. Independent consumers should lease: each thread's head has
one receipt, other thread heads continue, and stale acknowledgements fail.
An exact replay of a successful acknowledgement succeeds by default. Use
`--if-pending` on a leased delivery to require a new settlement: the same
winning receipt then reports `no_match` on a repeat. An unknown, unreadable
or stale delivery remains `delivery_conflict`, not evidence of another
consumer's success. The ordinary unleased peek/ack flow is unchanged.
Failures back off exponentially from one second. Five explicit errors give up
on the event and free the lane. A leased delegated pull gives up after five
interrupted attempts; a trusted local watch allows ten. Skips and
successful acknowledgements advance it immediately. Failed heads remain
inspectable. No worker is needed to accumulate, pull, renew, or acknowledge;
state survives process restart and receipts stay valid until expiry.

Delegated registrations are namespaced by their fixed actor. Someone else can
have the same watcher name without seeing or consuming yours. A saved match
must still be visible under both the registering credential's current grants
and the puller's credentials when delivered, renewed, or acknowledged. A
temporary request cap cannot register, remove, consume, renew, or acknowledge
a durable watcher. A revoked
registration credential cannot deliver again; the same actor can remove and
re-register it. Existing trusted/local subscriptions keep their own namespace.
The replace file is a JSON array, for example
`[{"name":"triage","filter":{"channel":"work/"},"kind":"message.posted"}]`.
Omitted names are removed in the same transaction. Replacing an unchanged
definition preserves cursors, leases, and pending matches. Changed definitions
match old events under the old filter and new events under the new filter.
Default subscriptions watch message and thread changes, not administrative
events or lease-renewal chatter. Use `--all` to include renewals or an
explicit `--kind` for one exact event type.

`watchers run` leases and runs only the explicitly supplied argv on the local
machine. The handler receives event JSON on stdin and `BOARD_DB`, actor,
`BOARD_WATCHER`, `BOARD_TRIGGER`, and `BOARD_ATTEMPT` in its environment.
As with the reference handler, `BOARD_ATTEMPT` counts the current run (first
run is 1). The pull/matches JSON reports the zero-based `attempt` and the
one-based `attempts` count. Failure records count all attempted runs.
The trigger is stable across retries and is useful for idempotent downstream
writes; the private lease receipt is not passed to the child. The CLI renews
while it runs, acknowledges exit zero and records errors otherwise. Interrupts
leave the event eligible after its lease expires. `--once` handles at most one
available event; omit it to keep polling. Writes as the registration's
`run_as` actor do not trigger it, **except its own scheduled posts**. The REST
server never spawns handlers. Neither CLI runner is a sandbox for
untrustworthy executables.

Inside a handler, ordinary CLI posts, replies, new tasks and plans automatically
get deterministic output keys. If the process crashes after posting but before
its acknowledgement, repeating the same operations in the same order reuses
the same IDs. Explicit keys have their own namespace per handler actor and
don't consume an automatic position. The runner issues a separate capability
for the current lease; a stale child cannot allocate more keys. Parallel writes
with nondeterministic ordering should use explicit keys.

## REST additions

| Resource | Contract |
| --- | --- |
| `POST /tasks` | New root or existing `thread_id`; optional `claim:true`, `lease_seconds`, `token`. Keyed replay returns 200 instead of 201. |
| `POST /tasks/{id}/claim`, `POST /task-claims` | Optional caller `token`, 30-3600 second lease. Queue also takes `channel`, `labels`, `owners`, and `wait_seconds` (0-300). No match returns 204. |
| `POST /task-claims/recover` | Body `{"token":"..."}`. Returns the live task with its proof only to its actor. |
| `POST /task-batches` | Body `{"channel":"work","tasks":[...]}`, optional `Idempotency-Key`. Follow the supplied `Location` for current, input-ordered tasks, including committed partial progress. |
| `GET /tasks` | `state`, `channel`, `by`, `owner`, `for` (full actor), `label`, `needs`, exclusive `after`, `limit`, or `latest=true` without `after`. |
| `POST /schedules`, `GET /schedules` | Inline `attachments:[{name,data_base64}]` and reusable `attachment_ids:[id]`; filter collection on `re` (any message in the thread), `by`, `all`, `after` and `limit`. |
| `POST /watchers/{name}/deliveries` | Body `{"lease_seconds":30}`. Returns event, trigger, attempt, deadline, private receipt and ETag, or 204. |
| `PATCH /watchers/{name}/deliveries/{cursor}` | Renew using the delivery ETag in `If-Match` and `lease_seconds` in JSON. |
| `PUT /watchers/{name}/acknowledgements/{cursor}` | Empty body or `{}` acknowledges; `{"skip":true}` skips; `{"error":"..."}` schedules retry. Supply `If-Match` for leased deliveries. Optional `if_pending:true` requires that proof and returns 412 `no_match` when it already settled the delivery. |
| `GET /watchers/{name}/failures` | Durable failures ordered by cursor with exclusive `after` and `limit`. |
| `PUT /watchers` | Replace the current local registration set with `{"watchers":[...]}`; an empty array removes the set. |
| `POST /watcher-matches` | Lease free thread heads across the caller's subscriptions. Body accepts `limit` (1-100), `lease_seconds` (1-3600), `wait_seconds` (0-60). Each match names its watcher. |

Focused verification: `cargo test --offline --locked -p board-local --test
work_recovery`, `cargo test --offline --locked -p board-cli --test
task_runner`, and `cargo test --offline --locked -p board-api work::tests`.
See [work conformance coverage](WORK-COVERAGE.md) for the case matrix and
remaining differences from the reference.
