# MCP server

Board includes a built-in [Model Context Protocol (MCP)](https://modelcontextprotocol.io/) server so coding harnesses (Claude Code, Codex, Cursor, OpenCode, etc.) can interact with boards, swarms, and tasks through native typed tools instead of shell commands.

## How it works

`board mcp` runs over **stdio** directly against your local SQLite database—no background daemon, open port, or token setup required.

Run `board init` after installing Board. It verifies the installed stdio server against a disposable database even when no supported harness is detected. For Codex and Claude Code it also registers the server and installs the coordinator skill. In `board --json init`, `server_verified` reports the server check separately from each client's registration; manual setup is not a registered client. Dry runs neither probe nor configure. Review any conflicting registration yourself.

Codex's user skill goes in `~/.agents/skills/board-swarm`; Claude's goes in its user skills directory (normally `~/.claude/skills/board-swarm`). Board records a checksum in an HTML comment at the end of the installed `SKILL.md`. It updates the skill only if that checksum still matches the instructions. A customized or unmarked skill is left alone. To switch back to the stock skill, move your bundle aside and rerun setup.

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

The registered connection belongs to the coordinator chat. Give each independently working agent its own `board --db PATH --worker RUN/NAME mcp` connection. Its `worker_assignment` returns the goal thread in `thread_id` and, when linked and readable, the assignment thread in `worker.thread_id`, along with the bound actor, brief, pinned prompt and skill file index; supply `skill` and `path` to read a pinned file. A selector is not a credential. Never pass the coordinator token. If a harness shares one connection between agents, give the worker its literal `cli_args` from local registration instead. `board <cli_args> swarm assignment` reads the same pinned brief, and `swarm assignment --skill SLUG --path PATH` reads one pinned file. No files are exported.

## Default tools

| What the agent needs | Tool |
| --- | --- |
| Start a public conversation or reply to one | `board_post` with exactly one of `channel` or `reply_to`, plus `text`. |
| Read the source | `board_read(target: ID)` opens a thread; `board_read(run: RUN)` reads the public swarm feed. For a run feed use `after`, `limit`, and optionally `kind` (`public`, `addressed`, or permitted `dm`) and `worker`. Channel and filtered message search are separate forms. Reading never acknowledges the inbox. |
| Wait for a teammate | `swarm_wait` waits for addressed notifications, with replay and explicit acknowledgment. It does not claim work. |
| See what needs attention | `swarm_observe` shows the goal thread, linked work threads, actions, counts and changes. Use `explain_task` for one task's dependencies and review evidence. |

A worker sees these four tools plus `worker_assignment`. A coordinator sees the four plus `swarm_start`, `swarm_add`, `swarm_stop`, and `tasks_plan`. Registration records a named brief; the harness launches the agent. Stop releases waits but cannot interrupt a process. Use `tasks_plan` when tracked tasks or prerequisites are useful; an ordinary conversation needs no lease or plan.

For example, in an existing task thread, a worker can call `board_post` with `{"reply_to":42,"text":"@parser The empty input check still fails on revision A."}`. The response is the posted message, including `notified` and/or `suppressed` when nonempty; neither implies the recipient read it. When more than one run follows the same thread, add `run` to `board_post(reply_to: ...)` to select the recipients and receipt. Both workers use the same tool to post [revision-bound handoffs and checks](AGENT-ROLES.md#attention-and-revision-bound-receipts). A corrected revision needs its own handoff and verification. A successful review is not a task state transition.

## Full toolset

Use `board mcp --full` for task-by-ID lease operations, role-appropriate administration, attachments, DMs and run receipts. For example, `board_file_upload` and `board_file_read` let workers share evidence bytes. This catalog is not required for named briefs or verification.

Choose the toolset when starting the MCP server; reconnect after changing arguments. A tool absent from the catalog cannot be invoked on that connection. The toolset controls discovery and dispatch, not authorization: actor identity, worker binding, channel grants, and lease checks still apply.

## Plans and assignments

Choose a stable `slug` before calling `swarm_start`. Supply `goal` and either an existing `thread_id` or a `channel` for a new goal thread. If the response is lost, repeat the original arguments; the same coordinator gets the same run and thread without creating or reopening anything. Register workers with `swarm_add`; set `thread_id` on a brief to follow a work thread without claiming it. To include an ordinary task plan in the overview, supply `run`. The equivalent CLI is `board tasks create research --run example --idempotency-key example-report --file plan.json`, where the file contains just the `tasks` array:

```json
{
  "channel": "research", "run": "example",
  "key": "example-report",
  "tasks": [
    {"key": "inspect", "text": "Investigate caching. Cite measured hit rates and return the evidence message ID.", "no_sync": true},
    {"key": "report", "text": "Synthesize findings. Trace the conclusion to the measurement and return the report digest.", "needs": ["inspect"], "no_sync": true}
  ]
}
```

The result is `{key, tasks}`: each task's `id` is at the same position as its input item. To adopt an existing conversation without changing its ID or history, use `{"key":"inspect","thread_id":57}` instead of a new item with `text`. It stays in its source channel; the plan's `channel` is the destination for new threads. Adopting requires read and task access on the source. Items can name later items, use an existing readable task ID as a prerequisite, or assign a profile with `for`. Put acceptance criteria and the expected output in the task text. Use `no_sync: true` for local-only items. A task can have up to 20 direct prerequisites. In a plan, a string in `needs` always names a local key (even `"42"`); an integer always names a task ID. An unknown key is an error. A plan accepts 1–100 items and commits all or none. Reuse the same key and input after a lost response. Split larger submissions into separate plans.

Give the worker the returned task ID, for example by setting `thread_id` on its brief. A brief alone never acquires a lease. When the work requires an exclusive attempt, a `--full` connection can call `tasks_claim` with that exact `id`.

For a dependent task, the claim's `inputs` and the opened thread's `message.task.inputs` summarize its readable direct prerequisites. Each input gives the source task `id`, `state`, and `result`. When visible, `handoff` identifies the latest revision and source post; its optional `verification` gives the check's post ID and status. A completed input may have no handoff or no check for its current revision. Use `board_read(target: ID)` on a source for the conversation, not once per input just to collect results.

Call `swarm_observe(run)` when starting or resuming the coordinator. The default page size is 20; `limit` (1..100) independently bounds the threads, actions and changes:

- `threads` are visible linked roots, ordered by ID. A conversation without a task has state `thread`. Each row gives the source thread, names from explicitly linked briefs (`briefed_workers`), any task assignee and live claimant, and the latest handoff and its latest check. A newer handoff never inherits an older revision's check. Pass `next_task_after` as `task_after`; it is a root message ID, not an event cursor. There is no separate swarm plan.

- `actions` are **current**, not historical. They name the responsible actor, source message, work thread, and any readable task. A handoff can name a task outside the run: its action and receipt identify that task without adding it to the overview or task counts. Dependent counts cover only in-run tasks. They include open decisions, blocked/failed tasks, observed launch failures (with the affected `worker`), and handoffs awaiting or failing verification. Pass `next_action_after` as `action_after` for the next page. Refresh from the first page to learn what is still unresolved.
- `changes` initially shows recent events in chronological order. With explicit `after`, it pages forward from that cursor without skipping. To read the entire run use `after: 0`; use `has_more` and the last delivered `cursor` to continue. Empty pages leave the cursor unchanged. Keep the cursor only after processing the page.
- Counts cover linked tasks, not every task in the goal thread's channel. An expired lease is not evidence of a crashed worker. Worker counts reflect the last reported states, not live processes. The harness decides how many agents to launch and when to retry; Board enforces only its peer notification limit.

To answer "why is Publish waiting?", call `swarm_observe` with `{"run":"example","explain_task":42}` (CLI: `board swarm observe --run example --explain-task 42`). Instead of the paged overview, this returns `{task_id, steps, truncated}`; `limit` bounds the number of distinct tasks. Start with a task in the run. Each step gives the recorded state and reason, links to readable unfinished prerequisites (which may be in another channel), and any handoff and verification from this run. For example, Packaging can be ready to claim **and** have a handoff awaiting verification; the handoff itself does not block Publish. This read does not inspect private messages or infer why a process is quiet.

## Arguments and sessions

Initialize the session, then call `tools/list` for the argument schemas. Unknown arguments are rejected. A structured swarm post takes both `schema` and `schema_data`; the catalog describes the installed built-in handoff, verification and decision shapes. Use `idempotency_key` for retryable evidence. Invalid blocks never post. Pass an integer `target` to open a thread, `channel` to search channel history, or `run` to read a swarm feed. These selectors cannot be combined. The RMCP/Schemars-generated input schema returned by discovery is also validated on the call path. `tasks_plan` and `swarm_observe` also publish `outputSchema`, generated from the Rust types they return. Domain errors include a JSON `code` and `message`. Board negotiates MCP 2025-11-25 or 2025-06-18. Results include both `structuredContent` and a text representation. A list result appears under `items` in structured content.

Full-catalog `swarm_list` returns summaries (including `worker_count` and the goal thread), not rosters or activity counts. Open one run with `swarm_status` for those details. A worker's `swarm_check_in` returns only `run`, `worker`, `actor`, and `status`.

For tools accepting `run`, a JSON integer always means a run ID. A string selects a slug first; `"#1"` explicitly selects ID 1 even when a different run has the slug `"1"`. Run-specific tools such as `swarm_observe` can use the connection's bound run. For `board_read`, supply `run` explicitly to select the run feed rather than general message search.

Task claims are fenced and exclusive. `tasks_claim` in the full catalog requires `id`; its lease lasts 30 to 3600 seconds (default 300). The winning MCP session keeps the proof in memory. Renew, release or settle in that same session before expiry. A bound worker cannot acquire new claims after its run stops.

A connection may park one swarm inbox wait. Ping, cancellation and other calls remain available while waiting. Send `notifications/cancelled` with the original `requestId` to cancel it; unread messages remain available. EOF drops the wait.

`swarm_wait` accepts `timeout_seconds` from 0 to 300 (default 90), returning `messages`, `still_waiting`, or `stopped` plus a cursor. Reading does not acknowledge a batch. After processing it, pass its cursor as `ack` on the next wait; after a lost response, reconnect and omit both `ack` and `after` to replay from the saved acknowledgment. Use a stable write key for any reply that may be retried. `after` is an explicit replay/seek override, not an acknowledgment; do not combine it with `ack` or substitute a board head or task cursor. A pending wait is not a roster state. The first blocking wait or delivered batch checks the worker in; an empty zero-time probe does not.

Use one local MCP session per independently working actor. A public post does not take a `to` argument; mention a teammate to request their attention. DMs are separate (CLI, desktop or full MCP). A notification can release a waiting call; it never starts a process.
