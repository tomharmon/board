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

The registered connection belongs to the coordinator chat. Give each independently working agent its own `board --db PATH --worker RUN/NAME mcp` connection. Its `worker_assignment` returns the bound actor and run, brief, pinned prompt and skill file index; supply `skill` and `path` to read a pinned file. A selector is not a credential. Never pass the coordinator token. If a harness shares one connection between agents, use `swarm_add(export_local: true)` and give the worker its literal CLI arguments instead.

## Default tools

| What the agent needs | Tool |
| --- | --- |
| Start a public conversation or reply to one | `board_post` with exactly one of `channel` or `reply_to`, plus `text`. |
| Read the source | `board_read(target: ID)` opens a thread; `board_read(run: RUN)` reads the public swarm feed. For a run feed use `after`, `limit`, and optionally `kind` (`public`, `addressed`, or permitted `dm`) and `worker`. Channel and filtered message search are separate forms. Reading never acknowledges the inbox. |
| Wait for a teammate | `swarm_wait` waits for addressed notifications, with replay and explicit acknowledgment. It does not claim work. |
| See what needs attention | `swarm_observe` shows the run goal, task threads, actions, counts and changes. Use `explain_task` for one task's dependencies and review evidence. |

A worker sees these four tools plus `worker_assignment`. A coordinator sees the four plus `swarm_start`, `swarm_add`, `swarm_stop`, and `tasks_plan`. Registration records a named brief; the harness launches the agent. Stop releases waits but cannot interrupt a process. Use `tasks_plan` when tracked tasks or prerequisites are useful; an ordinary conversation needs no lease or plan.

For example, in an existing task thread, a worker can call `board_post` with `{"reply_to":42,"text":"@parser The empty input check still fails on revision A."}`. The response is the posted message, including `notified` and/or `suppressed` when nonempty; neither implies the recipient read it. Both workers use the same tool to post [revision-bound handoffs and checks](AGENT-ROLES.md#attention-and-revision-bound-receipts). A corrected revision needs its own handoff and verification. A successful review is not a task state transition.

## Optional toolsets

- **Shared queue:** Start only participating connections with `board --db PATH --worker RUN/NAME mcp --queue` (or `board mcp --queue` for an unbound client). This adds `tasks_claim`, `tasks_renew`, `tasks_release`, `tasks_complete`, `tasks_fail`, and `tasks_block`. Useful when interchangeable workers pull from a backlog, not when the brief already identifies who should do the work. See the pool example below.
- **Full compatibility:** For an existing integration that calls the previous catalog, use `board mcp --full` (client config: `"args":["mcp","--full"]`). This retains `board_reply(id: ...)`, `swarm_post` and `swarm_read` with their existing reply shapes and defaults, plus the previous role-appropriate tools for files, DMs, worker status, run receipts, task administration, etc. The ordinary CLI and desktop features are unchanged. For example, upload evidence bytes with `board_file_upload` and read them with `board_file_read` in full mode; this does not belong on every worker's tool list.

Choose the toolset when starting the MCP server; reconnect after changing arguments. A tool absent from the catalog cannot be invoked on that connection. The toolset controls discovery and dispatch, not authorization: actor identity, worker binding, channel grants, and lease checks still apply.

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

From a worker connected with `--queue`, call `tasks_claim` with `{"ready_channel":"swarms/example","labels":{"pool":"research"},"wait_seconds":30}`. One eligible worker gets the task, even if several are waiting. Renew before `lease_until`; if the lease is lost, stop that attempt. Finish with a durable output reference or release unfinished work. Waiting on the queue does not consume a mention or DM.

For a dependent task, the claim's `inputs` and the opened thread's `message.task.inputs` summarize its readable direct prerequisites. Each input gives the source task `id`, `state`, and `result`. When visible, `handoff` identifies the latest revision and source post; its optional `verification` gives the check's post ID and status. A completed input may have no handoff or no check for its current revision. Use `board_read(target: ID)` on a source for the conversation, not once per input just to collect results.

Call `swarm_observe(run)` when starting or resuming the coordinator. The default page size is 20; `limit` (1..100) independently bounds the threads, actions and changes:

- `threads` are ordinary task roots, ordered by ID. Each row links its task thread, current assignee and live claimant, latest handoff and latest check of that handoff. A newer handoff never inherits an older revision's check. The overview groups the current page using the ordinary `workstream` label (or existing `pool` label). Pass `next_task_after` as `task_after`; it is a task ID, not an event cursor. There is no separate swarm plan.

- `actions` are **current**, not historical. They name the responsible actor, source message, linked in-run task, and number of in-run tasks directly waiting on it. They include open decisions, blocked/failed tasks, observed launch failures (with the affected `worker`), and handoffs awaiting or failing verification. Pass `next_action_after` as `action_after` for the next page. Refresh from the first page to learn what is still unresolved.
- `changes` initially shows recent events in chronological order. With explicit `after`, it pages forward from that cursor without skipping. To read the entire run use `after: 0`; use `has_more` and the last delivered `cursor` to continue. Empty pages leave the cursor unchanged. Keep the cursor only after processing the page.
- Counts cover tasks in the run channel; an expired, not-yet-reclaimed lease is `expired`, not evidence of a crashed worker. Other queues and their dependent counts remain separate. Available capacity subtracts checked-in working/waiting workers, not unknown harness processes. Account for in-flight launches in the chat. Policy is guidance for the coordinator, not OS process enforcement.

To answer "why is Publish waiting?", call `swarm_observe` with `{"run":"example","explain_task":42}` (CLI: `board swarm observe --run example --explain-task 42`). Instead of the paged overview, this returns `{task_id, steps, truncated}`; `limit` bounds the number of distinct tasks. Start with a task in the run. Each step gives the recorded state and reason, links to readable unfinished prerequisites (which may be in another channel), and any handoff and verification from this run. For example, Packaging can be ready to claim **and** have a handoff awaiting verification; the handoff itself does not block Publish. This read does not inspect private messages or infer why a process is quiet.

## Arguments and sessions

Initialize the session, then call `tools/list` for the argument schemas. Unknown arguments are rejected. Labels are JSON objects mapping strings to strings. A structured swarm post takes both `schema` and `schema_data`; the catalog describes the installed built-in handoff, verification and decision shapes. Use `idempotency_key` for retryable evidence. Invalid blocks never post. `board_read.target` accepts a message ID for a whole thread or a channel name for filtered history. Domain errors include a JSON `code` and `message`. For negotiated protocol versions from 2025-06-18, results also have `structuredContent`; list-only results use an `items` object wrapper there. The original JSON text (including bare lists) remains available.

For tools accepting `run`, a JSON integer always means a run ID. A string selects a slug first; `"#1"` explicitly selects ID 1 even when a different run has the slug `"1"`. Run-specific tools such as `swarm_observe` can use the connection's bound run. For `board_read`, supply `run` explicitly to select the run feed rather than general message search.

Task claims are fenced and exclusive. `tasks_claim` takes exactly one of `id` or `ready_channel`; its lease lasts 30 to 3600 seconds (default 300). Only a queue can specify `wait_seconds` (0..300, default 0); timeout is the ordinary `nothing_ready` tool error, not run completion. A winning MCP session keeps the proof in memory, never in tool output, including renewal. Renew or settle **in the same server session** before expiry. After a process dies another session must wait for expiry and claim again, even with the same profile. A bound worker cannot acquire new claims after its run stops.

A connection may park one inbox wait and one ready-queue wait concurrently. Ping, cancellation and other calls (including renewal) remain available on that connection. Cancel an outstanding request with `notifications/cancelled` and its original `requestId`. Canceling a parked inbox wait leaves unread messages available. A claim already committed before cancellation remains subject to its normal lease. EOF drops outstanding waits; there is no background runner.

`swarm_wait` accepts `timeout_seconds` from 0 to 300 (default 90), returning `messages`, `still_waiting`, or `stopped` plus a cursor. Reading does not acknowledge a batch. After processing it, pass its cursor as `ack` on the next wait; after a lost response, reconnect and omit both `ack` and `after` to replay from the saved acknowledgment. Use a stable write key for any reply that may be retried. `after` is an explicit replay/seek override, not an acknowledgment; do not combine it with `ack` or substitute a board head or task cursor. Upgrading keeps existing inbox positions.

Use one local MCP session per independently working actor. A public post does not take a `to` argument; mention a teammate to request their attention. DMs are separate (CLI, desktop or full MCP). A notification can release a waiting call; it never starts a process.
