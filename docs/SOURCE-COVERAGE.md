# Source coverage

These are the seven static sync cases in reference 0.54.0, not seven runs of
its transport suite. [Storage tests](../crates/board-core/tests/sync.rs),
[provider HTTP tests](../crates/board-sync/src/tests.rs), [additional native
payload fixtures](../crates/board-sync/src/tests/coverage.rs) and the
[two-backend workflow comparison](../scripts/compare-workflows.mjs) run independently.

| Original check | Local contract and evidence |
| --- | --- |
| 1. Imported author's profile, origin, retry, single root, target receipts, lookup | Source-qualified authors, stable thread/message/copy/permalink mappings. The workflow comparison exercises an import, retry, second-root rejection and receipt lookup. `incoming_replay_origin_edit_delete_and_offline_receipts_survive_restart` also exercises precise reply targets and stale edits. The original allows attaching a deliberately private root; this port refuses. |
| 2. Learn a mentioned provider actor who has not written | Canonical source-qualified mentions register only attributed people actually mentioned in the text. Workflow comparison and `automatic_slack_files_profiles_attribution_status_and_file_removal_use_confirmed_copies` (HTTP fixture). |
| 3. Inbox uniqueness, raw payload, per-thread FIFO and fenced ack | Queue, raw input, effect, receipt and checkpoint live in SQLite. `competing_inbox_consumers_take_distinct_lanes_and_fence_receipts` and `a_failed_inbound_head_fences_only_its_lane_until_reconciled`; the workflow comparison separately leases and acks both backends. |
| 4. Long inbound edit and boundary event ID | A normalized envelope is distinct from a user-authored message. Preserve its text on the queue; accept a 300 UTF-16 unit key, reject 301. The workflow comparison reads the leased payload and checks the key boundary on both backends. |
| 5. Deliver message changes to sync | Instead of a hidden reserved watcher, each enabled source gets its own transactional outbox. Leases, redelivery and per-thread order work with no in-process listener. `independent_mirrors_forward_imports_not_echoes_and_never_export_private_threads`, `disconnect_after_a_remote_send_requires_reconciliation_after_lease_expiry`, and `source_renderer_rereads_current_labels_only_updates_changed_text_and_fences_a_failed_start`. |
| 6. Keep the do-not-sync bit on roots and replies | `--no-sync` survives restart; private roots suppress all descendants, manual linking, file jobs and backfill. Proven by the independent-mirrors storage test and the inspector's guarded private composer tests. |
| 7. Narrowed caller cannot import or inspect raw inboxes and receipts | All source operations recheck owner, channel rights and integration narrowing inside the storage operation. No raw inbox data in ordinary reads. [Delegation tests](../crates/board-cli/tests/delegation.rs) exercise both CLI and loopback HTTP. |

**Additional evidence:** signed native intake, Slack file upload/permission
fallback, membership checks before preview/join, conditional public link
snapshots, dropped-import responses, GitHub review/CI and Linear comment
parentage are tested with local HTTP fixtures. Public GitHub read/replay is
the only live provider test. No credentials, deployment or remote writes are
needed for this suite. See [connector operations and limits](SYNC.md).
