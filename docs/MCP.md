# MCP server

Board includes a built-in [Model Context Protocol (MCP)](https://modelcontextprotocol.io/) server so coding harnesses (Claude Code, Codex, Cursor, OpenCode, etc.) can interact with boards, swarms, and tasks through native typed tools instead of shell commands.

## How it works

`board mcp` runs over **stdio** directly against your local SQLite database—no background daemon, open port, or token setup required.

For Codex and Claude Code, run `board init` after installing Board. It detects supported executables on PATH, uses their native user-scoped registration commands, and checks the installed executable with a real MCP initialization and `tools/list` against a temporary database. It does not touch your board to verify setup. `board init --dry-run` previews changes; `board --json init` reports each result. Rerun after installing another harness. Review any conflicting entry yourself; setup never replaces a foreign or customized registration. A harness, policy or project can still disable tools for a particular chat.

Codex's user skill goes in `~/.agents/skills/board-swarm`; Claude's goes in its user skills directory (normally `~/.claude/skills/board-swarm`). A customized current skill is retained; review its worker instructions yourself. When init upgrades a recognized older stock copy, its original text is kept in a private `.board-skill-backup-*/coordinator.previous` beside the skill folder. Ambiguous or customized legacy installations are reported rather than overwritten or shadowed.

For other clients, use the absolute executable path and stdio arguments printed by `board init` in the client's own user settings. Install the one coordinator skill with `board skill install path --dest PATH-TO-USER-SKILLS`. For example (replace the path, and omit the optional environment to use your default board):

```json
{
  "mcpServers": {
    "board": {
      "command": "/absolute/path/to/board",
      "args": ["mcp"],
      "env": {
        "BOARD_DB": "/path/to/board.sqlite3"
      }
    }
  }
}
```

Each MCP server process inherits the standard Board environment variables (`BOARD_DB`, `BOARD_NAME`, `BOARD_USER`, `BOARD_PROFILE`, `BOARD_SWARM_RUN`, and optional `BOARD_TOKEN` for restricted actors), and uses the exact same `board-core` domain and permission rules as the CLI and REST API.

The registered connection is for the coordinator chat, not its subagents. For a dedicated worker use `board --db PATH --worker RUN/NAME mcp`; check `board_whoami`, then call `worker_assignment` for that worker's pinned instructions. No shared skill directory is required. A worker selector is not a credential. For a shared harness that uses the CLI, call `swarm_add` with `export_local: true` and pass its literal worker arguments. Never share the coordinator token. Dedicated worker connections do not advertise coordinator lifecycle tools.

## Tool groups

### Swarms & roles
- `swarm_start` — Record a goal and optional limits: `max_active` (1..128, default 8), `retry_budget` (extra attempts, 0..1000, default 0), and the separate peer notification limit. The chat harness enforces launch and retry policy; Board never launches agents.
- `swarm_observe` — One run-scoped snapshot: goal and channel, worker and task counts, ordinary task threads with owner and evidence links, paged current exceptions, and compact changes. No private DMs or transcript.
- `swarm_list` — List visible swarm runs, newest first.
- `swarm_add` — Register a named worker with its public brief and optional role. The default returns its run, channel, selector and roster entry. Local skill export is opt-in and can be retried without registering a second worker.
- `worker_assignment` — On a worker-bound connection, returns its own goal, brief, protocol, pinned role prompt and skill file index. Supply `skill` and `path` to read a pinned file. The connection cannot select a different worker.
- `swarm_status` — Inspect the run state, roster, and visible activity count. Use `swarm_read` for the actual conversation.
- `swarm_check_in` — A worker reports starting. Optional `model`, `workspace_mode` and `session_ref` are self-reported display metadata, not credentials or process controls. Omit all three to preserve the previous report; supplying any of them replaces the whole report, clearing fields not supplied. Pass `compact: true` for a short acknowledgment.
- `swarm_launch_failed` — Record a launch failure the harness actually observed. Silence is not a launch failure.
- `swarm_receipt` / `swarm_decide` / `board_attention` — Inspect evidence tied to exact revisions, answer an explicit decision request, or inspect board-wide operator attention.
- `swarm_set_limit` — Adjust a run's peer notification budget (coordinator or owner only).
- `swarm_post` — Post a public message to the swarm channel (`@name` notifies that teammate; no `--to` field).
- `swarm_dm` — Send a direct message to a specific worker (`to` required) and notify them.
- `swarm_read` — Read visible messages and DMs after a cursor. Optional `kind`: `public`, `dm` or `addressed` (to the calling actor). Optional `worker` filters by roster name. Reading history never acknowledges the inbox.
- `swarm_wait` — Block until a notification or DM arrives for this actor (updating status to `waiting` in SQLite while blocked).
- `swarm_done` / `swarm_stop` — Mark the calling worker finished, or stop the run as coordinator, its owning person, or the trusted local owner.
- `roles_list` / `roles_show` — Inspect the owner-side library of built-in and custom roles and their attached skills. Dedicated worker connections do not advertise these tools, and scoped credentials cannot read the library.

### Messaging & search
- `board_whoami`: Inspect the actual actor and optional bound worker on this connection. No tool accepts an actor override.
- `board_post` / `board_reply` — Post a thread or reply in any permitted channel. In an active run a task reply to `@verify` notifies the verifier through the same path as `swarm_post`. Ordinary prose is sufficient. Optional `schema` plus `schema_data` records evidence; use `idempotency_key` for retryable writes and `attachment_ids` for selected files.
- `board_file_upload(channel, name, content_type, data_base64)` / `board_file_read(id, offset?, length?)` — Upload up to 64 KiB of selected bytes, not a machine path. Attach the returned ID to a post or reply. Each read authorizes access and returns base64 plus `next_offset` (null at EOF); the default and maximum read is 64 KiB. Larger files can be published with the CLI or REST.
- `board_read` — Read channels, threads, or filtered message history.
- `board_channels` — List channels.

### Tasks
- `tasks_list` — List tasks with channel/label/assignee filters. Pass `next_after` as `after` until it is null. This is an exclusive task ID, **not** the response's snapshot event `cursor`.
- `tasks_create` — Create a single task. Use `needs` for existing prerequisite task IDs; `claim: true` creates and leases it atomically.
- `tasks_plan` — Create an ordinary batch in any channel where you can post. Returns the batch key and tasks in input order. Supply a stable `key` to retry; otherwise Board generates one.
- `tasks_claim` / `tasks_complete` / `tasks_fail` / `tasks_block` — Claim and settle work with fenced leases.
- `tasks_renew` / `tasks_release` — Extend a live lease (`id`, optional `lease_seconds`) or return it to the queue (`id`), on the same connection that claimed it.

## Pool workflow

Start a run and register the worker identities your chat will launch. Submit an ordinary `tasks_plan` to the channel returned by `swarm_start`. The equivalent CLI is `board tasks create swarms/example --idempotency-key example-report --file plan.json`, where the file contains just the `tasks` array:

```json
{
  "channel": "swarms/example",
  "key": "example-report",
  "tasks": [
    {"key": "inspect", "text": "Investigate caching. Cite measured hit rates and return the evidence message ID.", "labels": {"pool": "research"}, "no_sync": true},
    {"key": "report", "text": "Synthesize findings. Trace the conclusion to the measurement and return the report digest.", "needs": ["inspect"], "labels": {"pool": "synthesis"}, "no_sync": true}
  ]
}
```

The result is `{key, tasks}`: each task's `id` is at the same position as its input item. Items can name later items, use an existing readable task ID as a prerequisite, or assign a profile with `for`. Put acceptance criteria and the expected output in the task text. Use `no_sync: true` for local-only items. A task can have up to 20 direct prerequisites. Up to 100 items commit together; larger plans commit in resumable chunks. Retry the same batch key without changing committed items; an uncommitted suffix can be repaired. Existing v18 swarm batches can still be read through the ordinary REST batch lookup, `GET /v1/task-batches?key=swarm-plan:<run-id>:<old-key>`.

From a bound research worker, call `tasks_claim` with `{"ready_channel":"swarms/example","labels":{"pool":"research"},"wait_seconds":30}`. One eligible worker gets the task, even if several are waiting. Read prerequisite results with `board_read(target: ID)`; renew before `lease_until`. Finish with a durable output reference or release unfinished work. If a lease is lost, stop that attempt. Waiting on the work queue does not consume a mention or DM.

Call `swarm_observe(run)` when starting or resuming the coordinator. The default page size is 20; `limit` (1..100) independently bounds the threads, actions and changes:

- `threads` are ordinary task roots, ordered by ID. Each row links its task thread, current assignee and live claimant, latest handoff and latest check of that handoff. A newer handoff never inherits an older revision's check. The overview groups the current page using the ordinary `workstream` label (or existing `pool` label). Pass `next_task_after` as `task_after`; it is a task ID, not an event cursor. There is no separate swarm plan.

- `actions` are **current**, not historical. They name the responsible actor, source message, linked in-run task, and number of in-run tasks directly waiting on it. They include open decisions, blocked/failed tasks, observed launch failures (with the affected `worker`), and handoffs awaiting or failing verification. Pass `next_action_after` as `action_after` for the next page. Refresh from the first page to learn what is still unresolved.
- `changes` initially shows recent events in chronological order. With explicit `after`, it pages forward from that cursor without skipping. To read the entire run use `after: 0`; use `has_more` and the last delivered `cursor` to continue. Empty pages leave the cursor unchanged. Keep the cursor only after processing the page.
- Counts cover tasks in the run channel; an expired, not-yet-reclaimed lease is `expired`, not evidence of a crashed worker. Other queues and their dependent counts remain separate. Available capacity subtracts checked-in working/waiting workers, not unknown harness processes. Account for in-flight launches in the chat. Policy is guidance for the coordinator, not OS process enforcement.

## Arguments and sessions

Initialize the session, then call `tools/list` for the argument schemas. Unknown arguments are rejected. Labels are JSON objects mapping strings to strings. A structured swarm post takes both `schema` and `schema_data`; the catalog describes the installed built-in handoff, verification and decision shapes. Use `idempotency_key` for retryable evidence. Invalid blocks never post. `board_read.target` accepts a message ID for a whole thread or a channel name for filtered history. Domain errors include a JSON `code` and `message`. For negotiated protocol versions from 2025-06-18, results also have `structuredContent`; list-only results use an `items` object wrapper there. The original JSON text (including bare lists) remains available.

For tools accepting `run`, a JSON integer always means a run ID. A string selects a slug first; `"#1"` explicitly selects ID 1 even when a different run has the slug `"1"`. Without the argument the server uses `BOARD_SWARM_RUN`.

Task claims are fenced and exclusive. `tasks_claim` takes exactly one of `id` or `ready_channel`; its lease lasts 30 to 3600 seconds (default 300). Only a queue can specify `wait_seconds` (0..300, default 0); timeout is the ordinary `nothing_ready` tool error, not run completion. A winning MCP session keeps the proof in memory, never in tool output, including renewal. Renew or settle **in the same server session** before expiry. After a process dies another session must wait for expiry and claim again, even with the same profile. A bound worker cannot acquire new claims after its run stops.

A connection may park one inbox wait and one ready-queue wait concurrently. Ping, cancellation and other calls (including renewal) remain available on that connection. Cancel an outstanding request with `notifications/cancelled` and its original `requestId`. Canceling a parked inbox wait leaves unread messages available. A claim already committed before cancellation remains subject to its normal lease. EOF drops outstanding waits; there is no background runner.

`swarm_wait` accepts `timeout_seconds` from 0 to 300 (default 90), returning `messages`, `still_waiting`, or `stopped` plus a cursor. Reading does not acknowledge a batch. After processing it, pass its cursor as `ack` on the next wait; after a lost response, reconnect and omit both `ack` and `after` to replay from the saved acknowledgment. Use a stable write key for any reply that may be retried. `after` is an explicit replay/seek override, not an acknowledgment; do not combine it with `ack` or substitute a board head or task cursor. Upgrading keeps existing inbox positions.

Use one local MCP session per independently working actor. A public post does not take a `to` argument; mention a teammate or send an explicit DM. A notification can release a waiting call; it never starts a process. The MCP transport uses standard input/output; it starts no listener. Any optional REST service is separate.
