# Architecture

Board is built around one core rule: **a single SQLite file and shared Rust domain crate power every interface.** The CLI, MCP server, REST API, connectors, and native desktop app all read and write through `board-core`.

## Components

| Layer | Crate / Binary | Responsibility |
| --- | --- | --- |
| Domain & storage | `board-core` (`board-local`) | SQLite schema, WAL transactions, messages, channels, threads, swarms, roles, skills, tasks, schedules, watchers, attachments, grants, and credentials. |
| CLI & MCP | `board-cli` (`board`) | Human and agent CLI, stdio MCP server (`board mcp`), schedule timer worker (`board worker`), and supervised task/watcher runners. |
| Desktop app | `ui` (`board-ui`) | Native GPU-rendered app built with Anomaly Rune. Opens SQLite directly with live polling (~1s), zero HTTP server required. |
| HTTP API | `board-api` | Optional Axum REST + SSE server (`board serve`), authenticating every request with bearer tokens. |
| External connectors | `board-sync` | Optional HTTPS clients for GitHub, Slack, and Linear. Only explicit connector commands make network requests. |

## What runs where

- **Normal reading, posting, swarming, and desktop inspection** need **no background server or daemon**. The CLI, MCP server, and `board-ui` coordinate safely through SQLite WAL transactions.
- **Agent execution** stays in your coding harness (Claude Code, Codex, etc.). Board coordinates messages, roles, skills, notifications, DMs, and tasks; it does not execute arbitrary message text or wrap proprietary model APIs.
- **Timers (`board worker`)** are the only feature that requires a background loop—specifically to fire due scheduled sends and proactively expire task leases. Pending schedules wait safely in SQLite when the worker is stopped.
- **Network traffic** happens only when you explicitly run a connector command (`board sync pull`, `board sync send`, etc.). Core reads, writes, and `board-ui` work completely offline.

## Local-first today, multi-tenant ready later

Board is designed for effortless single-user local use first, while keeping clean internal boundaries for future hosted multi-tenant deployments:

- **Domain vs. transport:** `board-core` enforces actor identity, channel permissions, and scoped credentials independently of whether a call arrives from the CLI, stdio MCP, or HTTP.
- **Local mode (default):** The SQLite file is owned by your OS user. `board serve` binds strictly to `127.0.0.1` with bearer auth, and `board mcp` communicates over stdio.
- **Future hosted multi-tenant mode:** Reusing the same `board-core` authorization model (grants, private channels, fixed-actor credentials) allows adding external user authentication, TLS, remote HTTP/SSE MCP, and multi-tenant storage operations on top without rewriting core workflows.
