# Reference coverage

Comparison target: reference commit `c2d3324b117adaa8e9a0107e15ea2fca207c95ac`
(package 0.54.0). The audit includes the README, SPEC, local backend, CLI
docs, inspector, integration code and conformance checks. The reference
declares 65 protocol methods and 90 static conformance cases (28 core, 2
paging, 2 search, 2 schedules, 24 tasks, 14 grants, 5 schemas, 3 attachments,
3 watchers, 7 sync). **These are inventory counts, not Rust test results.**
Resource URLs and JSON shapes differ; an upstream client is not supported.
See [the verification checklist](../PARITY.md) for tests that actually run.

| Reference family | Rust coverage | Verification / qualification |
| --- | --- | --- |
| discovery, me, channels, createChannel, actors | Discovery/OpenAPI inventory, identity, named boards, hierarchical scopes, paged channel summaries, directory of people, profiles and external authors. | [REST](../crates/board-api/src/tests.rs), [local access](../crates/board-core/tests/access.rs). Local profile verification is not provider account verification. |
| channelMessages, threads, message | Exact parent plus root; whole thread; scope, search, author, resolved mentions, repeated label predicates, cursor and event head for one SQLite snapshot. | [Snapshot, paging and deletion tests](../crates/board-core/tests/messages.rs), [compound REST filters](../crates/board-api/src/tests.rs). |
| post, posted, edit, delete, react, label | Stable ID, recovered response for actor/target/key, compare-and-set text and labels, newest-only reply, reactions and persistent tombstones. | [Message tests](../crates/board-core/tests/messages.rs) and [processes](../crates/board-cli/tests/processes.rs), including concurrent writes and a fault between mutation and event. |
| events | Transactional ordered history, event-time filtering, current-view historical tail, long poll/SSE, resume and wait from the posted ID. | [Feed/access tests](../crates/board-core/tests/access.rs), [polling and SSE revocation](../crates/board-api/src/tests.rs). Restore a SQLite snapshot, not a reconstruction from the public feed. |
| schemas, schema, createSchema | Immutable versioned local registry and bounded JSON Schema subset. Validate blocks in posts and edits; examples in code fences aren't data. | [Schema rules](../crates/board-core/tests/schema_contract.rs), [prose](../crates/board-core/tests/actors_and_prose.rs), [direct comparison tool](../scripts/compare-reference.mjs). |
| upload, attachment, uploadStart, uploadFinish | Inline BLOBs, named staged files, resumable offset-checked parts, reusable handles, authorization on upload/reuse/download. | [File and restart tests](../crates/board-core/tests/uploads.rs), [binary REST resources](../crates/board-api/src/files_api.rs). No hosted presigned URLs. |
| tasks, createTask(s), createdTasks, claim(Ready)Task, renew/release/complete/fail/block/unblock/reopen/assign/setNeeds/cancel | Thread-backed ownership and dependencies, atomic adopt/create-and-claim, recoverable fenced leases, supervised argv, atomic small DAG plans, larger checkpointed plans, cycles refused. | [Storage](../crates/board-core/tests/work_recovery.rs), [two workers including a real clock expiry](../crates/board-cli/tests/processes.rs), [CLI runner](../crates/board-cli/tests/task_runner.rs). |
| schedule, schedules, cancelSchedule | Durable one-shot/repeat, individual reply targets, files, bounded catch-up, owner/credential recheck and cancellation. | [Schedule crash/injected transaction failure](../crates/board-core/tests/automation_delivery.rs), [blind creation and revocation](../crates/board-core/tests/work_recovery.rs). |
| watchers, register, matches, ack | Local subscriptions per actor, precise filters, acknowledgments, independent thread heads, expiring receipts, failures, stable callback trigger and explicit local handler. | [Watcher delivery](../crates/board-core/tests/automation_delivery.rs), [actual handler subprocess](../crates/board-cli/tests/task_runner.rs). Not remotely hosted callbacks. |
| grants, createGrant(s), updateGrant, explain, revokeGrant | Local actor/channel rules, longest match, atomic grant batches with recovery receipts, effective explanation, expiry, revocation, narrowing and owner-managed credentials. | [Core](../crates/board-core/tests/access.rs), [CLI and authenticated server](../crates/board-cli/tests/delegation.rs). Single local owner is the authority, no verified organization roles or installation principals. |
| syncImport, syncReceipt, syncThread, syncFind, syncCopies, inboxPush/Next/Ack, inbox, readSource, joinSource, synced, linked | Source identity and receipts; immutable copies, local/remote lookup, idempotent inbox, checkpointed pull, per-thread outbox, manual link/start/backfill, external preview and explicitly confirmed public Slack join. | [SQLite](../crates/board-core/tests/sync.rs), [real HTTP against fixtures](../crates/board-sync/src/tests.rs), [provider verification](SYNC.md#verification-and-limits). Offline local intake replaces a publicly reachable webhook. |
| typing and inspector | Expiring status, drafts and unread tracking, chronological activity, rich thread/task and administration views, deep links, keyboard navigation, continuous cross-process reads. | [Scripted native rendering](../ui/tests/inspector.rs), [CLI/HTTP/live Rune/restart](../scripts/roundtrip.py). No web front end. |

## What does not transfer one-for-one

- **Runtime and transport:** no shared deployment, public webhooks, managed
  wake service, cross-team identity lookup, public avatar proxy, browser
  inspector, hosted function/model execution, or upstream TypeScript
  middleware. Their useful local counterparts are explicit worker/handler
  processes, fixed-actor credentials, stored profile metadata, native Rune
  views, and a Rust embedding API. There is no requirement for a separate
  database, broker, container engine or Git host.
- **Provider fidelity:** the connectors import and publish conversations, not
  every provider-specific feature. In particular, importing all private file
  bytes automatically (Slack copies are explicit), rich marks/identity impersonation, provider-wide avatar lookup,
  and reconstructing events the remote API no longer returns are not
  implied by a successful conversation pull. Comparisons of all provider
  webhook shapes, callback orders or account-specific permissions have not
  been run. Ordinary Board attachments and local reactions work offline.
- **Guards against accidental sharing:** local root posts are not
  automatically announced to a provider. Explicitly enable publishing and
  link/start the intended conversations. An ambiguous write is not
  automatically retried. OS file access is a trusted boundary, not a
  security sandbox. Native tests and release smoke checks were run on
  Linux, not on macOS or Windows.

For another release of the reference, repeat the audit rather than inferring
parity from a shared method name or line count.
