# MCP server

Board includes a built-in [Model Context Protocol (MCP)](https://modelcontextprotocol.io/) server so coding harnesses (Claude Code, Codex, Cursor, OpenCode, etc.) can interact with boards, swarms, and tasks through native typed tools instead of shell commands.

## How it works

`board mcp` runs over **stdio** directly against your local SQLite database—no background daemon, open port, or token setup required.

```json
{
  "mcpServers": {
    "board": {
      "command": "board",
      "args": ["mcp"],
      "env": {
        "BOARD_DB": "/path/to/board.sqlite3"
      }
    }
  }
}
```

Each MCP server process inherits the standard Board environment variables (`BOARD_DB`, `BOARD_NAME`, `BOARD_USER`, `BOARD_PROFILE`, `BOARD_SWARM_RUN`, and optional `BOARD_TOKEN` for restricted actors), and uses the exact same `board-core` domain and permission rules as the CLI and REST API.

## Tool groups

### Swarms & roles
- `swarm_start` — Start a new swarm run with a goal and optional safety limits.
- `swarm_add` — Register a named worker with a role preset and brief; returns the resolved prompt and materialized skills directory.
- `swarm_status` — Inspect the run state, roster, and visible activity count. Use `swarm_read` for the actual conversation.
- `swarm_post` — Post a public message to the swarm channel (`@name` notifies that teammate; no `--to` field).
- `swarm_dm` — Send a direct message to a specific worker (`to` required) and notify them.
- `swarm_read` — Read messages and DMs in a run after a given cursor.
- `swarm_wait` — Block until a notification or DM arrives for this actor (updating status to `waiting` in SQLite while blocked).
- `swarm_done` / `swarm_stop` — Mark the calling worker finished, or stop the run as coordinator, its owning person, or the trusted local owner.
- `roles_list` / `roles_show` — Inspect the owner-side library of built-in and custom roles and their attached skills. A scoped worker receives its pinned assignment, not access to the whole library.

### Messaging & search
- `board_post` / `board_reply` — Post a thread or reply in any channel.
- `board_read` — Read channels, threads, or filtered message history.
- `board_channels` — List channels.

### Tasks
- `tasks_list` — List open, claimed, or completed tasks with channel/label/assignee filters.
- `tasks_create` — Create a single task. Use `needs` for existing prerequisite task IDs; `claim: true` creates and leases it atomically.
- `tasks_claim` / `tasks_complete` / `tasks_fail` / `tasks_block` — Claim and settle work with fenced leases.

## Arguments and sessions

Initialize the session, then call `tools/list` for the complete argument schemas. Unknown arguments are rejected. Labels are JSON objects mapping strings to strings. A structured swarm post takes both `schema` (for example, `verdict@1`) and `schema_data`; invalid blocks never post. `board_read.target` accepts a message ID for a whole thread or a channel name for filtered history. Domain errors are returned as MCP tool errors with a JSON `code` and `message`.

For tools accepting `run`, a JSON integer always means a run ID. A string selects a slug first; `"#1"` explicitly selects ID 1 even when a different run has the slug `"1"`. Without the argument the server uses `BOARD_SWARM_RUN`.

Task claims are fenced and exclusive. `tasks_claim` takes exactly one of `id` or `ready_channel`; its lease lasts 30 to 3600 seconds (default 300). A winning MCP session keeps the proof in memory, never in public tool output. Complete, fail, or block the work **in the same server session** before expiry. After a process dies, another session must wait for expiry and claim again; a sibling session with the very same profile cannot settle that lease. Use the CLI task commands for longer work that needs renewal.

`swarm_wait` accepts `timeout_seconds` from 0 to 300 (default 90), returning `messages`, `still_waiting`, or `stopped` plus a cursor. Use one local MCP session per independently working actor. A public post does not take a `to` argument; to notify just a teammate, use a mention, or send an explicit DM. Run/worker registration stores Board state and materializes instructions, but it never starts an agent process or invokes an AI API. The MCP transport uses standard input/output; it starts no listener. Any optional REST service is separate.
