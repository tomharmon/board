# Source coverage

These are the seven static sync cases in reference 0.54.0, not seven runs of
its transport suite. [Storage tests](../crates/board-core/tests/sync.rs),
[provider HTTP tests](../crates/board-sync/src/tests.rs), [additional native
payload fixtures](../crates/board-sync/src/tests/coverage.rs) and the
[two-backend workflow comparison](../scripts/compare-workflows.mjs) run independently.

| Original check | Local contract and evidence |
| --- | --- |
| 1. Imported author's profile, origin, retry, single root, target receipts, lookup | Source-qualified authors, stable thread/message/copy/permalink mappings. The workflow comparison exercises an import, retry, second-root rejection and receipt lookup. `incoming_replay_origin_edit_delete_and_offline_receipts_survive_restart` also exercises precise reply targets and stale edits. The original allows attaching a deliberately private root; this port refuses. |
| 2. Learn a mentioned provider actor who has not written | Canonical source-qualified mentions introduce only attributed people actually mentioned in the text. Subsequent mentions do not overwrite a known person's profile, including an existing author without a custom name. The author's own update can rename them; a sparse profile does not discard a known avatar. Workflow comparison, `source_mentions_can_introduce_but_only_an_actors_own_writes_can_rename_them` and `automatic_slack_files_profiles_attribution_status_and_file_removal_use_confirmed_copies` (HTTP fixture). |
| 3. Inbox uniqueness, raw payload, per-thread FIFO and fenced ack | Queue, raw input, effect, receipt and checkpoint live in SQLite. `competing_inbox_consumers_take_distinct_lanes_and_fence_receipts` and `a_failed_inbound_head_fences_only_its_lane_until_reconciled`; the workflow comparison separately leases and acks both backends. |
| 4. Long inbound edit and boundary event ID | A normalized envelope is distinct from a user-authored message. Preserve its text on the queue; accept a 300 UTF-16 unit key, reject 301. The workflow comparison reads the leased payload and checks the key boundary on both backends. |
| 5. Deliver message changes to sync | Instead of a hidden reserved watcher, each enabled source gets its own transactional outbox. Leases, redelivery and per-thread order work with no in-process listener. `independent_mirrors_forward_imports_not_echoes_and_never_export_private_threads`, `disconnect_after_a_remote_send_requires_reconciliation_after_lease_expiry`, and `source_renderer_rereads_current_labels_only_updates_changed_text_and_fences_a_failed_start`. |
| 6. Keep the do-not-sync bit on roots and replies | `--no-sync` survives restart; private roots suppress all descendants, manual linking, file jobs and backfill. Proven by the independent-mirrors storage test and the inspector's guarded private composer tests. |
| 7. Narrowed caller cannot import or inspect raw inboxes and receipts | Source administration, raw queues and receipt inspection recheck the owner, channel rights and integration narrowing in storage. The public read-only projection below requires only read in the message's channel; it does not return queue contents. [Delegation tests](../crates/board-cli/tests/delegation.rs) use both CLI and loopback HTTP. |

## Offline copy lookup

A source can start a conversation or join one. `sync start` stages
the local root and backlog. `sync join-thread` records a link and a
publication boundary without staging any text. The same distinction applies
to `post --sync SOURCE` and `post --sync SOURCE:THREAD`. The latter never
turns an existing issue or remote parent into a writable copy of the local
root. See the [start/join CLI regression](../crates/board-cli/tests/processes.rs)
and [v12 recovery test](../crates/board-core/tests/sync.rs).

`board sync targets MESSAGE` and `GET /v1/messages/{id}/sync` return the
message, its root, snapshot cursor, and the recorded thread destinations.
Each destination reports a source, target, remote thread and message IDs
when known, a safe **recorded** link when available, and a delivery state.
An unresolved start has no remote thread yet; a private local reply inside
a linked thread has `local_only` rather than a fabricated delivery.
No raw provider error, lease receipt, signed query URL or job payload is
exposed. Credentials do not belong in configured source names or IDs.

`board sync locate URL` and `GET /v1/links?url=...` resolve a local message
and root without naming the source. Percent-encode the URL as an HTTP
query parameter. Besides stored exact links, the resolver understands
GitHub comment/review/inline IDs and Slack archive timestamps against known
local copies; matching a timestamp alone cannot cross Slack workspaces.
Custom and Linear source URLs must have a matching stored permalink.
Resolution never fetches a URL. Nonexistent, unsupported and inaccessible
links all yield `message_not_found`. When one exact URL identifies multiple
readable threads, the port returns `ambiguous_link` instead of choosing an
arbitrary copy. Local output uses separate `source`/`target`, integer IDs
and bounded state names, not upstream SDK result objects.

Evidence: `compatible_provider_permalinks_resolve_offline`,
`public_targets_do_not_confuse_queue_lane_names`,
`read_only_copy_projection_resolves_all_readable_entries` and the real
CLI/REST `read_only_sync_lookups_and_schedule_scopes`. The regression
`private_lifecycle_reply_is_never_mirrored_or_backfilled` checks that an
imported conditional signal can update a mirrored root's labels while
keeping its attributed `no_sync` reply out of every other source, including
after restart, retry and explicit backfill.

Explicit Linear joins cannot authorize publishing to an arbitrary issue.
When an operator sends, the connector makes a metadata-only query before
each write to an existing issue and checks its current team. For a comment
edit/reaction or a nested reply it checks the comment's issue too. A failed
check leaves a retryable unsent job, not an uncertain mutation. The fixtures
`linear_explicit_joins_verify_the_current_team_before_any_mutation` and
`linear_rejects_a_mapped_parent_comment_belonging_to_another_issue`
inspect every request body, verify the allowed send and reject a moved or
out-of-team issue without sending any message text.

**Additional evidence:** signed native intake, Slack file upload/permission
fallback, membership checks before preview/join, conditional public link
snapshots, dropped-import responses, GitHub review/CI and Linear comment
parentage are tested with local HTTP fixtures. Public GitHub read/replay is
the only live provider test. No credentials, deployment or remote writes are
needed for this suite. See [connector operations and limits](SYNC.md).
