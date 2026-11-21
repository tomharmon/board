# Reference coverage

Comparison target: reference commit `c2d3324b117adaa8e9a0107e15ea2fca207c95ac`
(package 0.54.0). The audit includes the README, SPEC, local backend, CLI
docs, inspector, integration code and conformance checks. The reference
declares 65 protocol methods and 90 static conformance cases (28 core, 2
paging, 2 search, 2 schedules, 24 tasks, 14 grants, 5 schemas, 3 attachments,
3 watchers, 7 sync). **These are inventory counts, not Rust test results.**
Resource URLs and JSON shapes differ; an upstream client is not supported.
See [the verification checklist](../PARITY.md) for tests that actually run.

**Case-by-case audit:** [54 core, paging, search, grants, schema and file cases](CORE-COVERAGE.md);
[29 task, timer and watcher cases](WORK-COVERAGE.md); [7 sync cases](SOURCE-COVERAGE.md).
The [native inspector audit](INSPECTOR-COVERAGE.md) covers interactive workflows separately.
[Reproduce the direct comparisons](REFERENCE-CHECKS.md) to run both implementations,
not just the Rust tests. One original case calls for waking a deployed app: it
is mapped to a local handler, not counted as a successful deployment test.

## Latest pass: findings and fixes

The earlier inventory counted methods that existed but missed differences in
what callers could actually observe. This pass compared those boundaries again:

| Finding | Implemented behavior | Regression evidence |
| --- | --- | --- |
| A renewal or bookkeeping record could satisfy an ordinary wait; reconnecting with the original SSE URL could repeat its initial page. | Normal polling, history, wait and SSE now show conversation activity; an explicit kind or all-events view includes authorized bookkeeping. The resume cursor advances over skipped records. A reconnect after either kind of seed skips already delivered events. | [Real CLI waits](../crates/board-cli/tests/processes.rs), [SSE reconnect tests](../crates/board-api/src/tests.rs) and [both SQLite backends](../scripts/compare-workflows.mjs). |
| A source read required administrative rights; starting a root and linking it were separate commands. A lifecycle reply could escape a private import through a second mirror. | Read a recorded permalink or inspect public copy status with ordinary channel read access. Explicit post targets and durable outbox intents commit together. Joining an existing external conversation never invents a receipt or replaces the provider's original head. Local-only lifecycle signals never fan out, including on backfill. | [Atomic root/start/join and exact external parents](../crates/board-cli/tests/processes.rs), [source visibility and private signals](../crates/board-core/tests/sync.rs), [real CLI/REST projections](../crates/board-cli/tests/delegation.rs). |
| Channel counts and unfiltered thread pages rebuilt whole groups on reads. The actor directory omitted its snapshot cursor. A broad channel grant could expose unattached bytes. | SQLite v12 maintains indexed counts/activity in the write transaction. Directory rows and cursor come from one snapshot. A delegated reader of all channels cannot download or reuse someone else's unplaced upload. | [Populated database migration, query plans, tombstones and independent writers](../crates/board-core/tests/read_models.rs); [file authorization](../crates/board-core/tests/uploads.rs). |
| CLI and HTTP could create distinct actors for the same profile spelling; imported display names could not be mentioned reliably. | Both adapters use the same normalized personal profile. Provider-qualified display handles have a stable fallback, and incomplete later profile updates preserve known metadata. | [Eight original/Rust actor comparisons](../scripts/compare-profile-handles.mjs), [profile waits](../crates/board-cli/tests/processes.rs), [storage cases](CORE-COVERAGE.md). |
| Thread label filters, large file attachments and timer queries depended on which adapter was used. | Thread filters inherit each root label. CLI attachments up to 100 MiB use staged local uploads when needed. A committed post retry does not reread a removed file. Timers can be selected by thread, schedule ID or channel scope and created from a schema block. | [REST thread reads](../crates/board-api/src/tests.rs), [large attachments, filters and named timers](../crates/board-cli/tests/processes.rs), [schedule paging](WORK-COVERAGE.md). |
| Channel/task drafts, keyboard editing and incremental scroll differed in the native inspector. | Independent, permission-checked SQLite draft slots; guarded task creation; Markdown shortcuts, Up-to-edit, scoped autocomplete, and scroll anchoring. | [Scripted Rune typing, two windows, restart, revocation and wheel input](../ui/tests/inspector/composing.rs); [visual boundaries](INSPECTOR-VISUALS.md). |
| Media that played in the reference was a download or a short silent sample. | The native inspector previews GIF/WebP animations and AVIF stills, seeks throughout supported MP4/MOV and WebM video, and can enable local audio. Viewing never requires a network or HTTP server. | [Framebuffer pixels, timeline seek, audio process lifecycle and live grant revocation](../ui/tests/inspector/video.rs); [animation and AVIF pixels](../ui/tests/inspector.rs). Decoder, size and device limits are [explicit](../ui/README.md). |
| The reference distinguishes starting a new provider conversation from joining an existing one (SPEC section “The sync events”). A join implementation incorrectly queued the local root as a remote comment. | Start publishes the root; join records only a binding and an event cursor. Posting with a target or joining a thread later never shares pre-join messages, including via an edit or backfill. An actual copy receipt is a separate operation. | [CLI start versus join, older and newer replies](../crates/board-cli/tests/processes.rs); [canonical REST Location and all three providers](../crates/board-api/src/tests.rs); [upgrade fences unsent v12 roots](../crates/board-core/tests/sync.rs); [Linear sends only the subsequent reply](../crates/board-sync/src/tests.rs). |
| A future page cursor could skip writes that had not happened yet; a stale reply draft could prevent the adjacent task form from saving. | Empty message, thread and source queue reads clamp to the actual head of that snapshot. Native task and reply forms checkpoint independently. A strict acknowledgement can distinguish a still-open delivery from a replay without sacrificing ordinary retry recovery. | [Storage reads](../crates/board-core/tests/read_models.rs), [source queues](../crates/board-api/src/tests.rs), [two native editors after restart](../ui/tests/inspector/composing.rs), [strict versus replay-safe receipts](WORK-COVERAGE.md). |
| A moved Linear issue could leave an old delivery outside its configured team. | Before recording a send attempt, the connector reads the issue's current canonical ID and team; for a reply it also checks the comment's issue. A failed check leaves the provider untouched. This does not make a remote read and write atomic. | [Loopback provider protocol tests](../crates/board-sync/src/tests.rs); [source and connector boundaries](SOURCE-COVERAGE.md). |

The table below is the full method-family checklist. The [verification
record](../PARITY.md#verification-record) reports actual runs, separately
from the count of reference methods.

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
| grants, createGrant(s), updateGrant, explain, revokeGrant | Subject sets, exact/family/wildcard precedence, scoped management without escalation, atomic batches with recovery receipts, effective explanation, expiry, revocation and narrowing. | [Policy](../crates/board-core/tests/grant_policy.rs), [CLI and authenticated server](../crates/board-cli/tests/delegation.rs). The local owner is the root authority; no verified organization roles or installation principals. |
| syncImport, syncReceipt, syncThread, syncFind, syncCopies, inboxPush/Next/Ack, inbox, readSource, joinSource, synced, linked | Source identity and receipts; immutable copies, local/remote lookup, idempotent inbox, checkpointed pull, per-thread outbox, manual link/start/backfill, external preview and explicitly confirmed public Slack join. | [SQLite](../crates/board-core/tests/sync.rs), [real HTTP against fixtures](../crates/board-sync/src/tests.rs), [provider verification](SYNC.md#verification-and-limits). Offline local intake replaces a publicly reachable webhook. |
| write middleware and outgoing renderers | Owner-installed bounded Rhai programs: reject or rewrite before commit, emit additional messages atomically, optionally answer a dropped import, render source copies using current labels. | [Rules](../crates/board-core/tests/write_rules.rs), [rendered copy changes](../crates/board-core/tests/sync.rs), [protocol tests](../crates/board-sync/src/tests/coverage.rs). No arbitrary server-side JavaScript. |
| typing and inspector | Expiring status, guarded private/scheduled drafts with bytes, unread tracking, chronological activity, media and stored link previews, deep links, real keyboard navigation and cross-process reads. | [Scripted native rendering](../ui/tests/inspector.rs), [CLI/HTTP/live Rune/restart](../scripts/roundtrip.py). No web front end. |

## What does not transfer one-for-one

- **Runtime and transport:** no shared deployment, public webhooks, managed
  wake service, cross-team identity lookup, public avatar proxy, browser
  inspector, or hosted function/model execution. Their local counterparts
  are explicit workers and handler processes, scoped credentials, locally
  stored profile metadata, native Rune, a Rust embedding API and bounded
  write rules. A rule is ported, not executed as TypeScript. There is no requirement for a separate
  database, broker, container engine or Git host.
- **Provider fidelity:** the connectors import and publish conversations, not
  every provider-specific feature. Slack files are opt-in, bounded and
  independently receipted; plain-text attribution does not impersonate users.
  Round-tripping every rich-text mark, provider-wide avatar lookup,
  and reconstructing events the remote API no longer returns are not
  implied by a successful conversation pull. Comparisons of all provider
  webhook shapes, callback orders or account-specific permissions have not
  been run. Ordinary Board attachments and local reactions work offline.
- **Guards against accidental sharing:** nothing publishes by default. To
  mirror new roots, explicitly enable both publishing and root mirroring at
  a suitable source; GitHub issues can only be started deliberately. Local-only
  roots and their replies never enter the outbound queue. An ambiguous write is not
  automatically retried. OS file access is a trusted boundary, not a
  security sandbox. Native tests and release smoke checks were run on
  Linux, not on macOS or Windows.

For another release of the reference, repeat the audit rather than inferring
parity from a shared method name or line count.
