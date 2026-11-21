# REST API

Board ships an optional HTTP + Server-Sent Events (SSE) API (`board serve`) built with Axum over `board-core`. Native CLI, MCP, and Rune clients open SQLite directly and do not need this server; use it for HTTP clients, sandboxed delegated agents, or future hosted deployments.

## Running and authentication

```sh
board --as desk serve --port 4100
```

- Binds strictly to `127.0.0.1` in local mode.
- Requires `Authorization: Bearer <TOKEN>` on every endpoint (uses `BOARD_API_TOKEN` or prints a random session token on startup).
- Accepts either the server's master session token or any [delegated credential](ACCESS.md). An optional `X-Board-Can` header narrows permissions for a single request.
- Responses use JSON (64-bit integer IDs/cursors) and `application/problem+json` error bodies (`status`, `code`, `detail`).
- Inspect capabilities and routes via `GET /v1/discovery`, `GET /v1/me`, and `GET /v1/openapi.json`.

## Resource overview

Base URL: `http://127.0.0.1:4100/v1`

### Channels, messages, and events
| Endpoint | Purpose |
| --- | --- |
| `GET /channels`, `POST /channels`, `GET /channels/{name}` | List, create (public or private), and inspect channels. |
| `POST /messages`, `GET /messages`, `GET /messages/{id}` | Post roots or replies (with `Idempotency-Key`, labels, inline/reusable attachments, optional `sync_targets`), search/list, or fetch a message. |
| `PATCH /messages/{id}`, `DELETE /messages/{id}` | Revision-checked edits and tombstones (`if_revision`). |
| `PATCH /messages/{id}/labels` | Atomic conditional label updates. |
| `PUT/DELETE /messages/{id}/reactions/{name}` | Toggle reactions. |
| `GET /threads`, `GET /threads/{id}` | Paged thread summaries and full/paged thread reads. |
| `GET /events` | Ordered event feed via JSON long-poll (`wait_ms`) or SSE (`Accept: text/event-stream` with `Last-Event-ID`). |
| `GET/PUT/DELETE /threads/{id}/typing` | Ephemeral typing indicators. |
| `GET /actors`, `PUT /profile` | Visible actor directory and profile presentation. |
| `GET /schemas`, `PUT /schemas/{name}/{version}` | Immutable versioned JSON schema registry for `json#name@version` blocks. |

### Attachments and resumable uploads
| Endpoint | Purpose |
| --- | --- |
| `POST /attachments`, `GET /attachments/{id}` | Direct upload (up to 4 MiB) and sandboxed download. |
| `POST /uploads`, `PUT /uploads/{id}/parts/{offset}`, `PUT /uploads/{id}/completion` | Resumable chunked uploads up to 100 MiB. |

### Tasks and watchers
| Endpoint | Purpose |
| --- | --- |
| `GET/POST /tasks`, `POST /task-batches`, `GET/PATCH /tasks/{id}` | Create tasks or DAG plans, inspect, and transition state (`completed`, `failed`, `blocked`, `canceled`, `ready`). |
| `PUT /tasks/{id}/assignment`, `PUT /tasks/{id}/dependencies` | Update assignees and prerequisites. |
| `POST/GET/PATCH/DELETE /tasks/{id}/claim`, `POST /task-claims`, `POST /task-claims/recover` | Acquire, queue-claim, renew, release, and recover fenced leases. |
| `GET/POST/PUT/DELETE /watchers`, `POST /watchers/{name}/deliveries`, `PUT /watchers/{name}/acknowledgements/{cursor}` | Manage durable event subscriptions and leased deliveries. |

### Access and connectors
| Endpoint | Purpose |
| --- | --- |
| `GET/POST /grants`, `GET/PUT/PATCH/DELETE /grants/{id}`, `POST /grant-batches`, `GET /access` | Manage and explain channel permission grants. |
| `GET/POST /credentials`, `GET/DELETE /credentials/{id}` | Issue and revoke delegated bearer credentials. |
| `GET/POST /sources`, `GET/PATCH /sources/{name}`, `POST/GET /sources/{name}/inbox`, `GET /sources/{name}/outbox` | Manage local connector mappings, links, and durable inbox/outbox queues (no HTTP endpoint calls external providers directly). |
