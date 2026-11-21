# Behavioral parity

This is a Rust implementation of the board model, not a protocol-compatible
server for the original client. The comparison is against the reference
`README.md`, `SPEC.md`, local backend, CLI reference and conformance cases.
The tests here exercise the Rust interfaces; they do not run the upstream suite.
The [protocol inventory](docs/PARITY-AUDIT.md) maps all reference method
families and distinguishes covered behavior, deliberately local alternatives
and provider-specific limits. No claim of byte-for-byte or SDK compatibility.

| Reference contract | Local design | Executable proof (test name) |
| --- | --- | --- |
| One thread, precise reply target, create channel on first use | A post's event cursor is its stable ID; root and parent links; hierarchical paths and per-channel counts | [message_workflow_profiles_attachments_and_tombstones](crates/board-core/tests/messages.rs); [composer_search_individual_reply_and_reaction](ui/tests/inspector.rs) |
| Attributed people/profiles, files, reactions, labels, blocks and tombstones | Author/owner checks; SQLite BLOBs; immutable, locally validated schemas; revision separate from version | [local_blocks_are_immutable_and_validate_posts_and_edits](crates/board-core/tests/messages.rs); [resources_have_locations_statuses_and_replay_safe_writes](crates/board-api/src/tests.rs) |
| Retry without duplicates; conditional revision, labels and newest reply | Board-wide key bound to author and exact placement, with comparisons inside the same immediate write transaction | [a_key_binds_to_the_exact_actor_and_placement_not_a_request_body and concurrency tests](crates/board-core/tests/messages.rs) |
| Mutation and event either both commit or neither does | Snapshot of the affected message captured in the transaction; events ordered by SQLite | [fault_during_event_snapshot_rolls_back_post_and_attachments](crates/board-core/tests/messages.rs); [failed_schedule_commit_retries_once_even_with_competing_workers](crates/board-core/tests/automation_delivery.rs) |
| Search, read, then follow without losing the handoff | One snapshot per read; distinct page key and snapshot head; event-time filters; ID wait scans from creation | [read_snapshot_matches_cursor_during_concurrent_posts](crates/board-core/tests/messages.rs); [cli_retry_wait_resume_and_online_backup](crates/board-cli/tests/processes.rs) |
| Task ownership, routing, lifecycle and dependencies | State on the thread root; atomic plans and graph changes; completing a prerequisite can ready its dependents in the same write | [task_owner_assignment_reopen_dependencies_and_reserved_labels](crates/board-core/tests/tasks.rs); [atomic_plan_with_forward_edges_replays_and_refuses_cycles](crates/board-core/src/automation/tests.rs) |
| Only the live claimant can settle | Immediate SQLite write lock, expiring epoch plus private token; expired holder stays fenced even under the same profile | [independent_processes_cannot_share_or_reuse_a_claim and real-clock takeover](crates/board-cli/tests/processes.rs) |
| Delayed/repeated sends survive downtime, can be canceled | Worker dispatches as the creator, records send ordinal and next due atomically; missed repeat slots coalesce | [recurring_sends_keep_ordinal_across_restarts_and_obey_stop_conditions and target cancellation](crates/board-core/tests/automation_delivery.rs) |
| Durable watchers filter, redeliver and acknowledge | Persist subscription and cursor; optional exclusive delivery receipt; stale ack cannot skip a match | [watcher_filters_use_event_time_not_the_current_thread and leased_delivery_is_exclusive_expires_and_fences_stale_acknowledgements](crates/board-core/tests/automation_delivery.rs) |
| Multiple interfaces on one board | CLI, API and native UI use the same storage crate. Rune is driven by real key/pointer input, including external writes while a thread is open | [CLI -> REST -> Rune and restart](scripts/roundtrip.py); [Rune PNG, tree, input, error states and navigation tests](ui/tests/inspector.rs) |
| Restricted identities and staged files | Fixed-actor bearer, transactional grants/batches, narrowing, revocation, scoped views, private reusable and resumable blobs | [cross-process grants and scoped tokens](crates/board-cli/tests/delegation.rs); [uploads](crates/board-core/tests/uploads.rs) |
| Recover after losing a claim response or stopping mid-plan | Preallocated proofs, durable per-actor local cache and dependency-ordered checkpoints | [work recovery and multi-chunk failure](crates/board-core/tests/work_recovery.rs); [supervised execution](crates/board-cli/tests/task_runner.rs) |
| Typed sources, links, receipts and independent delivery lanes | SQLite inbox/outbox fenced by lease; an unconfirmed external write cannot be silently retried; start/link/backfill explicitly | [receipts, crash after intent, cursor and competing consumers](crates/board-core/tests/sync.rs); [provider HTTP, offline events and byte transfer](crates/board-sync/src/tests.rs) |
| Live discovery and activity | OpenAPI route inventory exercised against router, discovery/me, SSE cursor/revocation, native activity/unread/drafts and expiring typing | [REST contract tests](crates/board-api/src/tests.rs); [Rune](ui/tests/inspector.rs) |

## Boundaries and gaps

- Filesystem access is the ultimate trust boundary. Optional delegated
  credentials restrict the domain interfaces, not programs with direct
  permission to write the file. The [access guide](docs/ACCESS.md) describes
  what a locally verified actor means. There is no team deployment mode.
- Watchers deliver data. Only explicitly invoking a local runner can execute
  operator-supplied argv. Nothing runs commands found in board messages.
- The SQLite database (projections, event log, blobs and automation state),
  not an external log or blob store, is the backup and restore unit.
- Resource URLs, JSON shapes and CLI flags belong to this port. IDs and cursors
  are JSON integers. Do not point a client for the upstream wire protocol at it.
- The [provider guide](docs/SYNC.md) distinguishes the anonymous live GitHub
  read from Slack/Linear and remote-write fixture tests. Secrets come from
  explicit environment variables, not a hosted secrets or OAuth service.
- The [native guide](ui/README.md) lists which actions belong to the UI
  and which secret-bearing operations stay in the CLI/API. Other operating
  systems have not been verified.

## Verification record

Run the [README verification commands](README.md#backup-and-test) to build
both executable targets offline from the workspace lockfile, check formatting,
deny Clippy warnings, run all Rust tests including the deliberately slow
30-second lease takeover, and exercise the standalone release binaries. No GPU
or display crate is in the CLI dependency graph.

On Linux x86_64 with Rust 1.99.1, the locked workspace run on 2026-11-21
passed **119 tests**: 64 core/storage, 16 REST, 13 CLI/process, 8 provider
protocol/intake, and 18 native headless Rune tests. Both debug and release
binaries built offline; workspace formatting and all-target Clippy passed
with warnings denied. The read-only public GitHub run applied 31 records
on its first pull and zero on replay. No external write was performed.

With a separate read-only reference checkout and Node 26, run
`node scripts/compare-reference.mjs /path/to/reference target/release/board`.
It calls the reference's own schema, fenced-block and prose-mention code and
compares the actual Rust CLI's acceptance, projections and search hits. It
does not copy or ship reference implementation code, install a dependency,
modify the reference or run the upstream wire conformance suite.
The release-binary comparison matched all 17 schema definitions, 13 block
fixtures and 16 mention-query results.

The [round-trip script](scripts/roundtrip.py) also ran from outside the checkout
with just the two installed release binaries. The CLI created message #1;
the API read it and replied with #2 as `ada/desk`. A real-time Rune input
mark confirmed the thread was already open before the HTTP write. Both texts
appeared in the [rendered native window](docs/inspector.png) and in its
element tree. The API and UI exited, then fresh CLI, API and UI processes
read the same IDs and text. No worker was used. Backup testing separately
verified live WAL contents, attachment bytes and watcher state.
