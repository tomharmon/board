# Local REST resources

Base URL: `http://127.0.0.1:4100/v1` (port is configurable). Send
`Authorization: Bearer TOKEN` on **every** request; JSON bodies need
`Content-Type: application/json`. The owner token acts as the handle selected
when starting `board serve`; a delegated token binds its fixed actor. An optional
`X-Board-Can` narrows, never widens, a request. See [access, profiles and uploads](ACCESS.md).
Message, task and event IDs are 64-bit integers. Credentials, external IDs,
source keys and upload handles may be strings.

`GET /discovery`, `GET /me` and `GET /openapi.json` provide local capability
discovery, the effective actor, and the OpenAPI 3.1 **route inventory**. The
inventory is checked against actual router methods; consult this guide and
the linked resource guides for field-level payload contracts. Common message,
task, schedule, source and write-rule write bodies also have machine-readable schemas.

## Messages and events

| Method and resource | Input / response |
| --- | --- |
| `GET /channels` | Optional `scope=studio`, lexical `after`, `limit`; returns channel counts, snapshot `cursor` and `next_after`. |
| `POST /channels` | `{"name":"studio/build"}`; 201 with Location, or 200 with `created:false`. First post creates its channel without a separate request. |
| `GET /channels/studio/build` | One channel summary. `GET /actors` lists visible actor IDs or, with `details=true`, their cards. Repeated `id` and optional `query` narrow the list; `cursor` belongs to that same snapshot. |
| `POST /messages` | Exactly one of `channel` or `reply_to`, plus `text`, optional `labels`, `attachments`, `attachment_ids`, `no_sync`, and (replies only) `after`. Shareable roots can include `sync_targets:["source","another:thread"]`: the first queues a new remote conversation, the second joins an existing one without sending the root. Both commit with the post or neither does. 201 with message and Location. Same key, author and target: 200 with the recorded message, not an edit or a new publication. Reuse by another author or target: 409. |
| `GET /posts/{key}` | Recover the current message for the local server author's client key; 404 for missing keys and other authors' keys. URL-encode the key. |
| `GET /messages/{id}` | One current message. `re` is the root ID for a reply; `reply_to` is the individual parent when different from the root. |
| `PATCH /messages/{id}` | `{"text":"Replacement","if_revision":5}`; revision condition is optional. Returns the updated message. |
| `DELETE /messages/{id}?if_revision=5` | Optional revision condition. Returns 204; the message remains readable as a tombstone. |
| `PATCH /messages/{id}/labels` | `{"set":{"state":"done"},"unset":["pending"],"conditions":{"state":"open","holder":null}}`. All conditions must hold together. `board.*` labels are reserved. |
| `PUT`, `DELETE /messages/{id}/reactions/{name}` | Add/remove the server actor's reaction. 204, including unchanged requests. |
| `GET /attachments/{id}` | Original bytes with their stored media type, `Content-Disposition: attachment` and `nosniff`; never a filesystem path. |
| `GET /messages`, `GET /threads` | A page of flat messages or summaries. Filters: `channel` (trailing slash for subtree), `text`, `by`, `re`, `mentions`, `from=SOURCE` (`board` for native posts), repeated `label=key` or `label=key=value`. |
| `GET /messages/{id}/deliveries` | Up to 100 most recent outbound jobs for this exact message at sources the caller may administer; statuses and reasons, not raw payloads or lease secrets. |
| `GET /rules`, `GET/PUT/DELETE /rules/{name}` | Trusted owner only. Install/update `{priority,script}`; 201 with Location when new. [Offline write rules](RULES.md). |
| `GET /threads/{id}` | Accepts any message in the thread. One snapshot: `{message,replies,schedules,cursor}`. Use repeated `label` predicates (each can match the root or reply), an `after` message from this thread and an optional reply `limit`. Omit these to read the whole thread. |
| `GET /events` | Ordered conversation changes. Without a cursor start at the live head; use `after=0` to start at the beginning. Add `all=true` for permitted bookkeeping, or name a specific `kind`. Same filters as lists; `wait_ms` takes 0..30000. Returns `{events,cursor}`; a timeout returns an empty page, not an error. |
| `GET /events` with `Accept: text/event-stream` | SSE event type, numeric ID and JSON data. Start at a cursor, or seed with the newest `last=1..100` matching events. On reconnect, `Last-Event-ID` skips the seed and resumes past the later of it and the original `after`. Credentials are checked again during delivery. |
| `GET/PUT/DELETE /threads/{id}/typing` | List active statuses, set yours (`{"status":"Reviewing","seconds":15}`), or clear. Presence expires and does not append a durable event. |
| `GET /schemas`, `GET /schemas/{name}/{version}` | Local schema definitions. New databases include `git-commit@1` and a provider-neutral `deployment@1`; an upgrade never replaces a site's own deployment schema. |
| `PUT /schemas/{name}/{version}` | `{"description":"...","schema":{"type":"object",...}}`. 201 with Location, 204 if identical; immutable definitions conflict. |

A message's `revision` changes only on edit/delete; `version` covers visible
mutations including reactions, labels and task changes. Send files in the same
transaction as the message, e.g.
`{"channel":"inbox","attachments":[{"name":"note.txt","data_base64":"aGk="}]}`.
Use standard padded Base64. A file is limited to 4 MiB; ten per message.
Structured blocks are Markdown fenced JSON tagged `json#schema-name@1`, not a
second unvalidated `blocks` input. For staged or resumable binary transfer use
[`/attachments` and `/uploads`](ACCESS.md); posted messages, tasks and
schedules accept reusable `attachment_ids`. Repeat query keys to intersect
label filters: `label=tag` tests presence, `label=tag%3D` tests an exact empty
value, and `label=stage%3Dtest%7Cdone` matches either named value.

Message and thread lists take `limit=1..500` (default 50). With no bounds,
read the newest page; pass its non-null `older` as `before` to go back.
With `after=0`, page forward by passing back each page's `cursor`; when
truncated it stops at the last returned match. `snapshot` is the event-log
head of that same read transaction, not the forward page key. `before` and
`after` are mutually exclusive. These are live lists, not frozen multi-request
snapshots. For an uninterrupted change stream, start `/events?after=SNAPSHOT`
and persist each processed cursor. IDs compared in application code should
use a 64-bit integer type. Don't assume adjacent IDs are both messages.

## Task resources

| Method and resource | Input / response |
| --- | --- |
| `GET /tasks` | Open tasks by default; optional `state` (including `all`), `channel`, `needs` (list dependents of this task), `label`, `after`, `limit`. |
| `POST /task-batches`, `GET /task-batches/{key}` | Example: `{"channel":"dev","tasks":[{"key":"a","text":"First"},{"text":"Next","needs":["a"]}]}`. Up to 100 items are atomic; larger submissions checkpoint. Supply an Idempotency-Key and use the response Location. Committed items cannot be changed by replay. |
| `POST /tasks` | `{"channel":"work","text":"Check","needs":[12],"assignee":"ada/qa","labels":{}}` or `{"thread_id":12}` to promote an existing root. New roots accept attachments and `Idempotency-Key`. Returns the task and its Location. |
| `GET /tasks/{id}` | Current state, owner, assignee, dependencies, holder, epoch, lease deadline, result. Never returns a claim secret. |
| `PUT /tasks/{id}/assignment` | `{"assignee":"ada/qa"}` or explicit `null` for a pool. |
| `PUT /tasks/{id}/dependencies` | `{"needs":[12,30]}`; replace all (empty array clears). Cycles are rejected atomically. |
| `POST /tasks/{id}/claim` | `{}` or `{"lease_seconds":300}`; 201, private `claim_token`, epoch and ETag. Lease duration 30..3600 seconds; default 900. |
| `POST /task-claims` | Claim from a queue: `{"channel":"work/","labels":{"step":"check"},"owners":["other"],"lease_seconds":300}`; 204 if nothing ready. |
| `GET /tasks/{id}/claim` | Active lease info, or 404; does not disclose the token. |
| `PATCH /tasks/{id}/claim` | Renew with `{"epoch":1,"token":"...","lease_seconds":300}`. |
| `DELETE /tasks/{id}/claim` | Release with `If-Match` set to the quoted ETag from successful acquisition. |
| `PATCH /tasks/{id}` | `{"state":"completed","epoch":1,"token":"...","result":"Done"}`; use `failed` with `reason`, or `blocked` with `reason` (a claimed task needs its receipt). Owner actions: `canceled` with `reason`, `ready` to unblock/reopen when allowed. Dependencies can put a resumed task in `waiting`. |

For work resource collections, `cursor` is the event-log head of the read.
Page with `next_after`, not that cursor: IDs for tasks/schedules, names for
watchers. Public task reads never expose a lost receipt. For a retryable
claim supply a fresh caller-generated secret; `POST /task-claims/recover`
recovers that actor's still-live receipt with `{"token":"..."}`. Never put
claim tokens in query strings. See [automation](AUTOMATION.md) for detailed
create-and-claim, plans, queue waits, schedule authorization and handler contracts.

## Schedules and watchers

| Method and resource | Input / response |
| --- | --- |
| `POST /schedules` | `{"channel":"work","text":"Check in","due":1893456000,"every":600,"times":3}` (use an actual due time within the next year). Or use `reply_to` instead of `channel`. Unix seconds; `every`, `times` or `until`, and `labels` are optional. |
| `GET /schedules`, `GET /schedules/{id}` | Pending collection by default; `all=true` includes completed/canceled entries. Filter before pagination by `channel` (a trailing slash includes the subtree), `re` (any message in a thread) or `by`. |
| `DELETE /schedules/{id}` | Cancel pending sends, 204. Does not delete the schedule's history. A worker fires messages; the API alone does not. |
| `POST /watchers` | `{"name":"qa","kind":"message.posted","filter":{"channel":"work/","labels":{"step":"check"}}}`. A new subscription begins at the current head. |
| `GET /watchers`, `GET`, `DELETE /watchers/{name}` | List, inspect or remove a subscription. |
| `PUT /watchers`, `POST /watcher-matches` | Atomically replace this actor's registrations with `{watchers:[...]}`, retaining cursors for unchanged filters; lease a bounded group with `{limit,lease_seconds,wait_seconds}`. |
| `GET /watchers/{name}/next` | Oldest unacknowledged match as an event, 204 if none available. Repeatable before ack; no exclusive lease in this mode. |
| `POST /watchers/{name}/deliveries` | `{"lease_seconds":30}` (1..3600); returns event, receipt, deadline and ETag, or 204 if unavailable. |
| `PUT /watchers/{name}/acknowledgements/{cursor}` | Ack precisely this delivery. Include its quoted ETag in `If-Match` for leased deliveries. No body for success, `{"skip":true}` to skip, or `{"error":"reason"}` to retry with backoff. An exact success replay returns 204 by default. Set `"if_pending":true` to require a new settlement: 412 `no_match` for the winning receipt after completion, 412 `delivery_conflict` for a stale, wrong or unknown delivery, 428 without the receipt. |
| `GET /watchers/{name}/failures` | Inspect failed deliveries after their retry budget; accepts `after` and `limit`. An outstanding event holds up only its own thread. |

Matches are based on the event-time message state, including entering/leaving
a filter. Pulling and acking don't run handlers or make recursive ack events.
Keep receipts private. A crash before ack permits redelivery, not a guarantee
of exactly-once external side effects.

## Source resources

These routes operate **only on local SQLite state**. No provider credential
belongs in an HTTP request to Board. See [connector examples and envelopes](SYNC.md).

| Method and resource | Input / response |
| --- | --- |
| `GET /links?url=...` | Resolve a recorded provider permalink to exact `message`, thread `root` and snapshot `cursor`. No network. Absent and unreadable links both return 404. |
| `GET /messages/{id}/sync` | Read-authorized targets, state and safe public copy URLs. Unlike administrative deliveries below, there are no raw payloads, errors or private receipts. Pending is not confirmation of delivery. |
| `GET/POST /sources`, `GET/PATCH /sources/{name}` | List/create `{name,provider,remote,channel}`. PATCH accepts `outbound`, `mirror_roots`, `mirror_files` (Slack only), `display_authors`, `github_items` (all/issues/pulls); omitted settings retain their values. Create returns 201 + Location (200 on identical replay). |
| `GET/PUT/DELETE /sources/{name}/renderer` | Trusted owner only. Read, install `{"script":"message.text"}` or clear a bounded Rhai renderer. A newly created renderer returns 201 with Location. |
| `POST /sources/{name}/imports` | Apply one normalized envelope and return the local message. |
| `PUT/GET /sources/{name}/links` | Attach `{thread,external,message,url?}`; find via `?thread=T&external=K`, just a thread, or `?url=...`. |
| `PUT/GET /sources/{name}/thread-links?thread=T` | Join an existing conversation with `{"root":123}`, or read its binding. Returns 201 with its canonical Location, 200 on an identical retry, 409 if already bound to another root. This is not a writable message copy: no local root, pre-join reply or edit to either goes to this target. |
| `GET /sources/{name}/copies/{id}` | All confirmed external copies for a local message at that source. |
| `GET/POST /sources/{name}/files/{id}` | Inspect confirmed file copies for a message; `{thread,attachment}` stages a deliberate Slack file share (202). No external operation is performed by the HTTP server. |
| `POST /sources/{name}/starts`, `POST /sources/{name}/backfills` | Explicitly stage a new conversation from `{root}` or unshared replies from `{thread}`. A start returns 202 and the queued delivery's Location. |
| `POST/GET /sources/{name}/inbox`, `GET /sources/{name}/outbox` | Enqueue an envelope (202 + Location) or list by exclusive `after`, `limit`, with a continuation `cursor`. |
| `POST /sources/{name}/deliveries`, `GET /sources/{name}/deliveries/{id}` | Lease `{"direction":"in","lease_seconds":60}` (or `out`). 201 with private receipt or 204 for no work. Outbound leases handed to external consumers record send intent first. |
| `PUT .../deliveries/{id}/receipt` | `{receipt}` for inbound, or include `result:{external,url?,thread?}` for a confirmed external write. |
| `POST .../deliveries/{id}/failure`, `PUT .../deliveries/{id}/resolution` | Report `{receipt,error,uncertain?,retry_after?}`; reconcile `{action:"retry"|"skip"|"delivered",result?}` after checking the remote state. |

## Failures

Errors, including malformed JSON, method errors and unknown routes, use
`application/problem+json` with `status`, `code` and `detail`. Branch on
status/code, not prose. Common statuses: 400 malformed input, 401 no bearer,
403 wrong owner/browser origin, 404 missing resource, 405 wrong method,
409 conflicting state/key/claim, 410 removed content, 412 stale HTTP
precondition, 413 body too large, 415 unsupported media type,
422 invalid data, 428 missing required precondition, 503 busy database.

```json
{"status":409,"code":"revision_conflict","detail":"revision_conflict: current revision is 9"}
```
