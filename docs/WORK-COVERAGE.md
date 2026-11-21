# Work conformance accounting

This maps the **24 task, 2 schedule and 3 watcher** entries in
`packages/board/src/conformance/index.ts` in the read-only reference checkout.
These are Rust behavioral tests, not a claim that this REST API runs the
TypeScript wire conformance suite unchanged. The reference checkout and
JavaScript runtime are not build or runtime dependencies.

The test names below are unique substrings usable as Cargo test filters.
Core tests are in `crates/board-core/tests/{tasks,automation_delivery,
work_recovery,work_parity}.rs` and `crates/board-core/src/automation/tests.rs`.
HTTP tests live in `crates/board-api/src/work/tests.rs`; real child-process
and CLI tests are in `crates/board-cli/tests/{processes,task_runner}.rs`.
Run `cargo test --offline --locked -p board-local -p board-api -p board-cli`.
The slow wall-clock takeover is explicit:
`cargo test --offline --locked -p board-cli --test processes -- --ignored`.

## Direct comparison

[compare-work.mjs](../scripts/compare-work.mjs) additionally runs the original
`createBoard` with its `local()` adapter and the compiled Rust CLI against two
independent temporary SQLite boards. It checks explicit expected outcomes
on each, then compares the two transcripts. IDs and times are compared
within each board, not as identical wire values. The original SDK's requests
stay in process; the Rust side starts a new CLI process for every operation.
Use a **scratch copy** of the original package with its dependencies installed
and a Node version with native TypeScript support (tested on Node 26):

```sh
cargo build --offline --locked -p board-cli
node scripts/compare-work.mjs /path/to/original/package-with-deps target/debug/board --expiry
```

The short run (omit `--expiry`) measures 41 equivalent outcomes: 11 watcher,
11 schedule, and 19 task observations. The wall-clock run adds three task
assertions after a real 30-second lease runs out, for **44 equivalent
outcomes**. These are not 43 upstream conformance cases. In particular:

| Family | What the two implementations actually did |
| --- | --- |
| Watchers | Replaced a registration, filtered by label and channel, leased independent thread heads, preserved leases across registration/reopen, rejected wrong and stale receipts, advanced one lane, retried a failed delivery and skipped it. |
| Schedules | Created and read a board-only reply schedule, scoped it to the right thread, canceled it, and reopened it. Both also dispatched a real one-shot with labels, its schedule ID, zero-based ordinal and board-only flag, then reopened without another send. The original uses its local timer; the Rust side invokes the worker. |
| Tasks | Claimed, retried a caller-minted proof without changing its deadline, renewed, released, failed, reopened, blocked and completed a task. The probe checked `by=me` against a caller and their separate profile, dependent readiness, omission of the proof from thread reads, and rejection of stale, wrong, too-short and terminal claims. With `--expiry`, the board is reopened during the lease; only the replacement holder can finish and its result persists. |

Three observed differences are asserted separately: the original first
watcher attempt is numbered 0, the port's is 1; repeating a successful
acknowledgement gives `no_match` upstream and succeeds idempotently here;
and the original SDK's renew result carries the proof, while Rust CLI renew
does not print it again. Both retain and accept the same proof and epoch
after renewal. This probe does not compare hosted delivery, external network
side effects, delegated policy matrices, repeated schedule cadence or native
UI behavior; the tests below cover the corresponding local workflows.
The comparison also caught a parser regression for otherwise valid
base64url proof values starting with a hyphen. The independent CLI test
`dash_prefixed_base64url_claim_proofs` exercises creation, recovery,
release, claim and hold with such proofs.
`reopening_a_claim_cache_preserves_another_connections_posix_lock` proves
that checking/opening an existing cache does not release another SQLite
connection's write lock in the same process: an independent writer remains
blocked until commit. `cache_files_are_private_regular_inodes_not_links`
checks the Unix mode and refuses shared or redirected proof files.

| Reference case, in upstream order | Local implementation / executable evidence |
| --- | --- |
| Schedule 1: create a reply, list by thread, cancel | Durable target and thread view; `schedule_and_watcher_resources_survive_server_and_worker_restart`, `cli_retry_wait_resume_and_online_backup`. |
| Schedule 2: keep board-only sends local | `scheduled_text_and_sync_policy_are_fixed` verifies both the durable flag and an empty outbox after an explicitly enabled connector is asked to publish the send; a comparable ordinary post is accepted. |
| Task 1: new/adopted task threads, label, summaries | `creation_adoption_and_claim_are_atomic`, `newest_tasks_are_selected_by_reply_activity`, `principal_profile_and_post_only_owner_rights`. |
| Task 2: keyed create versus plain roots/replies | Shared post identity; `single_task_is_a_keyed_post_but_a_batch_is_a_separate_guarded_request`. |
| Task 3: racing claims and result | Immediate SQLite transaction; `independent_processes_cannot_share_or_reuse_a_claim`, `concurrent_claims_have_one_winner`. |
| Task 4: lease deadline, minimum, renewal | 30-3600 seconds; `task_resources_page_replay_and_fence_claims`, `supervisor_renews_and_settles_child_exit`. |
| Task 5: fenced release, new epoch, fail/reopen | `dependencies_leases_and_stale_epochs`, `claim_secrets_never_leave_the_claim_response`, `expired_process_cannot_finish_after_a_real_time_takeover`. |
| Task 6: only successful prerequisites make work ready | `dependencies_leases_and_stale_epochs`, `task_owner_assignment_reopen_dependencies`. |
| Task 7: refuse canceled prerequisites; allow failed ones | `task_owner_assignment_reopen_dependencies`, `imported_edits_hold_adopted_task`. |
| Task 8: scope reverse-dependency list | `task_collection_finds_reverse_dependencies_and_filters_before_pagination`. |
| Task 9: latest-active window and reply data | `newest_tasks_are_selected_by_reply_activity`, `replacement_and_bulk_delivery_routes`: select after filtering, then return in chronological order. |
| Task 10: task.ready after create/unblock with already-completed prerequisites | `change_feed_and_watchers_include_both_sides`: explicitly inspects filtered durable events. |
| Task 11: replace, clear or retain existing dependencies | `task_state_and_assignment_resources_check_their_inputs`, `needs_checks_only_added_edges`. |
| Task 12: refuse direct and indirect cycles | `atomic_plan_with_forward_edges_replays_and_refuses_cycles`, `task_owner_assignment_reopen_dependencies`. |
| Task 13: reject changes to claimed or terminal work | Ownership and lifecycle checks in same transaction: `task_owner_assignment_reopen_dependencies`, `dependencies_leases_and_stale_epochs`. |
| Task 14: atomic multi-create with forward and existing edges | `atomic_plan_with_forward_edges_replays_and_refuses_cycles`, `task_batches_have_navigable_locations_and_atomic_keyed_retries`. |
| Task 15: invalid item rolls back entire batch | `failed_creation_rolls_back_thread_and_event`, `task_batches_have_navigable_locations_and_atomic_keyed_retries`. |
| Task 16: owner/claimant hold and release | `task_hold_kind_is_preserved`, `principal_profile_and_post_only_owner_rights`. |
| Task 17: exclusive directed assignment | `assignments_cannot_enqueue_another_persons_instructions_without_opt_in`. |
| Task 18: creator profile versus owner person | `principal_profile_and_post_only_owner_rights`: root author, exact task creator and task owner are distinct even on adoption. `legacy_profile_task_snapshots` checks an older profile's durable history after database migration. |
| Task 19: label permission required for claims | `principal_profile_and_post_only_owner_rights`, `scoped_tasks_hide_prerequisites_and_queue_limits`. |
| Task 20: unreadable needs are not usable | `scoped_tasks_hide_prerequisites_and_queue_limits`, `blind_task_and_plan_retries`. Internal named batch edges reveal only the writer's own new roots. |
| Task 21: exact-label subtree ready queue, waiting | `queue_labels_are_exact_and_owner_opt_ins_name_people`, `waiting_queue_claim_releases_connection`, `canceled_wait_before_commit`, `queue_poll_only_expires_the_overdue_lease_it_can_actually_take`. |
| Task 22: entering/leaving ready label; never expose token | `change_feed_and_watchers_include_both_sides`, `claim_secrets_never_leave_the_claim_response`. |
| Task 23: reserved task label and conditional updates | `task_owner_assignment_reopen_dependencies_and_reserved_labels`, `independent_writers_linearize_retry_revision_labels_and_latest_reply`. |
| Task 24: non-owner edit of imported root stops old lease | Actual local import, adoption, claim and reimport: `imported_edits_hold_adopted_task`. |
| Watcher 1: register, pull, acknowledge | `replacement_and_bulk_delivery_routes`, `leased_delivery_is_exclusive_expires_and_fences_stale_acknowledgements`. An exact repeat PUT is intentionally idempotent here, not a missing-match error. |
| Watcher 2: wake a deployed application by path | No deployment or network wake-up. `watchers run -- COMMAND` is the foreground local equivalent; `explicit_watcher_handler_receives_json`. |
| Watcher 3: temporary request cap cannot register, pull or ack | `watcher_set_replacement_preserves_leases_and_checkpoints_and_temporary_caps_cannot_pull_or_ack`. |

## Focused suites and operational differences

| Reference sources | Additional local evidence and boundary |
| --- | --- |
| `test/tasks.test.ts`, `cli-tasks.test.ts`, `cli-tasks-claims.test.ts`; task worker example in `docs/01-board/11-tasks.md` | Lease proof is recorded before the board write: `failed_ack_after_cli_claim_commit` injects a separate cache failure and recovers from independent processes. `supervisor_handles_interrupt_and_fences_children` exercises Unix process groups. `needs_checks_only_added_edges` builds two real deep graphs, crosses the 500-node guard, and checks that completing unreadable work cannot change a refusal. |
| `test/tasks-batch.test.ts`, `tasks-chunks.test.ts`, `cli-tasks-batch.test.ts` | First 100 are one transaction; larger plans commit in deterministic topological chunks. `large_plans_checkpoint_chunks` injects an error in a later chunk, reopens, claims committed work, repairs the uncommitted items, and checks idempotence. `multi_chunk_file_plan_round_trips` runs 101 tasks through separate CLI processes. Local SQLite has no remote adaptive transport-size boundary. Plans use IDs for previously uploaded attachments. There is no separate upstream task attach/detach transition; dependencies are replaced as a set. |
| `test/schedules.test.ts`, `cli-schedules.test.ts`, `local-scheduler.test.ts`; schedule guide | `failed_schedule_commit_retries_once_even_with_competing_workers`, `recurring_sends_keep_ordinal`, `new_schedule_checks_past_time`, `scheduled_files_survive_restart`, `blind_schedules_only_reveal_ones_created_blind`. Competing SQLite workers replace deployment-specific clock takeover and queue-delay hops. A reference scheduled reply normalizes to its root; the port additionally remembers an individual reply target. Deadlines use whole Unix seconds. |
| `test/watch.test.ts`, `http-watchers.test.ts`; watcher/triage examples | `watcher_lanes_run_independently`, `watcher_filters_use_event_time`, `interrupted_deliveries_resist_stale_ack`, `delegated_watchers_use_private_names`, `retrying_a_handler_emits_each_post_reply_task_and_named_output_once`. Local explicit argv receives JSON instead of a JavaScript handle. Only actual board writes get deduplication; file writes or other third-party effects require their own proof. |
| Reference adapter-specific task tests | Hosted query fanout, remote batch-size/lease clamps, platform identities, SDK request counts and deployment routing aren't SQLite limits. Local actor IDs use a person/profile separator. Resource paths, error code spelling, integer IDs and snapshot pagination are local contracts, not TypeScript wire compatibility. The port additionally binds every proof-backed task write to the exact actor and its current label permission. |

No tests here contact a third-party provider. The watcher worker does not
download or execute anything named by board content. Neither restoring the
board nor installing an optional connector enables outgoing delivery.

The port's native desktop composer additionally retires a saved draft
atomically with a schedule. `scheduling_a_saved_draft_is_atomic` injects
a SQLite write failure, refuses mismatched saved privacy, labels, file bytes
and send time, then exercises an outdated window, a successful commit, a
second submit and a database reopen.
