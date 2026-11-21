# Core contract coverage

These are semantic comparisons, **not a claim that an upstream SDK or its wire
conformance kit runs against this server**. The reference is the read-only
checkout's `packages/board/src/conformance/index.ts`, together with its
`contract/data-{schemas,values}.ts`, local backend, specification and CLI
documentation. Numbers below are one-based within each named group; they cover
all 54 checks in these six groups. The additional discovery handshake is
listed separately. Other groups have their own entries in [PARITY](../PARITY.md).

The Rust API is resource-oriented, spells fields in snake_case and uses
SQLite integer IDs/cursors, not opaque service IDs. The default source of
identity is the OS owner, not a team or a hosted token verifier.

## Messages and reading

Test names below are unambiguous prefixes in
[messages](../crates/board-core/tests/messages.rs),
[core conformance](../crates/board-core/tests/conformance_core.rs),
[access](../crates/board-core/tests/access.rs),
[REST](../crates/board-api/src/tests.rs), and
[actual CLI/HTTP](../crates/board-cli/tests/delegation.rs).

| Reference case | Local implementation and evidence |
| --- | --- |
| Core 1, 2: post, read by ID, reply | `Board::{post,reply,thread,get}`; stable root, thread and individual reply-to IDs. `message_workflow_profiles`, `resources_have_locations`. |
| Core 3: only if newest | Check and append under one SQLite writer lock; a deleted reply still counts for the guard. `independent_writers_linearize`, `idempotency_and_latest_guards`. |
| Core 4: client ID and posted lookup | The key identifies one actor and exact channel or reply target, not new text in a retry. `a_key_binds_to_the_exact_actor`, `idempotency_and_latest_guards`. Replaying a blind post cannot disclose other activity: `blind_own_retries`. |
| Core 5, 6, 7: versions, labels, revisions | Effective edits/reactions/labels advance event version; edits/deletes alone advance revision. Atomic compare-and-set for label values or absence, revision and latest checks; identical reaction/label operations do not manufacture changes. `independent_writers_linearize`, `profile_headers_edit_timestamps`, `message_workflow_profiles`. |
| Core 8, 11: channel message windows | Oldest-first forward pages resume at the last returned position when truncated; latest windows and their watermark describe a single snapshot. `subtree_search_event_snapshots`, `read_snapshot_matches_cursor`. |
| Core 9: paged channel prefix | Name ordering, literal prefix (e.g. `dev` also matches `developer`), distinct inclusive subtree option and channel-name resume. Visibility applied before limiting. `channel_prefix_pages_are_not_subtrees` and `scoped_cli_and_actual_loopback`. |
| Core 10: thread after/newest | Read via root or reply; root always present, limit counts replies, and another thread's cursor is rejected. `summaries_exclude_removed_replies`. |
| Core 12: reply count, latest | Live replies alone determine count/latest/activity ordering. Tombstones still exist in conversation reads. `summaries_exclude_removed_replies`. |
| Core 13: settle handles | Persist unique full handles as `@[actor-id]`. Report private response-only candidates for unresolved/ambiguous mentions; prefer appropriate thread participant; ignore code and invisible identities. `mentions_resolve_only_visible`, [prose tests](../crates/board-core/tests/actors_and_prose.rs). |
| Core 14: per-event cursor | Ordered durable log; normal forward feeds use immutable event-time snapshots. A failed write/event rolls both back. `subtree_search_event_snapshots`, `fault_during_event_snapshot`. |
| Core 15: newest events then follow | `event_tail` joins the **current** message for historical results, excludes deleted bodies and returns a snapshot continuation; following resumes live event-time delivery. `historical_tail_uses_current_content`, [CLI tail/process tests](../crates/board-cli/tests/processes.rs). |
| Core 16: long-poll | Poll does not retain a write lock while waiting, and releases its read connection. `pending_event_poll_releases_database`; CLI wait also catches an intervening reply in `cli_retry_wait_resume_and_online_backup`. |
| Core 17, 18: tombstones clear content and labels | Keep ID/reply links, clear current labels, blocks, files, reactions and text; label search cannot resurrect the deleted state. `message_workflow_profiles`, `summaries_exclude_removed_replies`. Forensic event snapshots are deliberately not erased. |
| Core 19, 20: channel/actor discovery and ensure | Channels on first post or explicit creation. Duplicate ensure has no event. Searchable scoped actor directory, including exact multiget. `channel_and_schema_retries`, `scoped_cli_and_actual_loopback`. |
| Core 21, 22: missing IDs and validation | Missing or inaccessible message ID is 404; malformed request syntax is 400. **Different wire contract:** well-formed invalid field values can be 422. `errors_and_access_boundary`, `scoped_cli_and_actual_loopback`. |
| Core 23: narrow a caller | Effective channel rules in `GET /v1/me`, request can only restrict rights; direct CLI and REST share live enforcement. `channel_rules_intersect_caps`, `scoped_cli_and_actual_loopback`. |
| Core 24, 25: profile avatar/ownership | `X-Board-Profile` selects name and optional HTTPS avatar; namespaced principal owns its profiles' posts, sibling profiles do not; a delegated profile cannot impersonate a sibling. `profile_headers_edit_timestamps`, `scoped_cli_and_actual_loopback`. |
| Core 26, 27, 28: ignore obsolete identity headers; no session | Separate avatar/session headers cannot create or change attribution. No session in viewer, current message or event. `scoped_cli_and_actual_loopback`. |
| Paging 1, 2: backwards windows, incompatible cursors | Group by thread before slicing; `older` and snapshot cursor are distinct. Reject backwards and forwards together. `older_windows_group_before_paging`. Consecutive pages are not a database-wide time-travel snapshot across concurrent mutations. |
| Search 1, 2: text in current roots/replies, channel subtree | Literal Unicode lowercase substring, not SQL wildcard; edits affect the current search and a descendant reply can select its root. Subtree includes root, excludes unrelated similar names. `channel_prefix_pages_are_not_subtrees`, `subtree_search_event_snapshots`. Also tested outside those two cases: visible author handles and `me` vs principal-family filters in current reads, events and history (`self_selectors_include_only`). |

Reaction identifiers are canonical ASCII names, including custom names. The
reaction oracle compares the reference's public names, glyphs and aliases to
the actual stored actor sets after a Rust CLI write (276 spellings on the
reference checkout used for this audit). Files and external SDKs are not used
to look up reactions at runtime.

Discovery handshake (separate from the 28 core cases): resource links at
`GET /v1/discovery`, OpenAPI at `/v1/openapi.json`. This is the local API's
own discovery document, not an upstream service address.

## Grants

See [grant policy tests](../crates/board-core/tests/grant_policy.rs),
[transaction and filtering tests](../crates/board-core/tests/access.rs),
[process test](../crates/board-cli/tests/delegation.rs) and [usage](ACCESS.md).

| Reference case | Local implementation and evidence |
| --- | --- |
| Grants 1, 2: create/list/revoke, distinct grants and membership | Independent numeric IDs, grouped subjects, replacement and atomic member add/remove. Repeated revoke reports no change. `most_specific_pattern_then_closest`, `scoped_cli_and_actual_loopback`. |
| Grants 3, 4, 5: empty rules, specificity, everyone | Explicit empty rule wins; exact channel before subtree before wildcard; exact actor before family before everyone at that pattern; union only ties. Everyone is not an admission grant. Explanation includes winning IDs for global managers. `most_specific_pattern_then_closest`. |
| Grants 6: team role caps | **Local replacement:** owner-issued immutable credential scope can cap a delegate at read even with wider live grants. `channel_rules_intersect_caps`. No upstream team membership, role lookup or multi-tenant account administrator. |
| Grants 7: application aliases, all-apps group | **Partial replacement:** explicit directory profiles, stable local actor IDs, multi-subject grants and principal families. No identity provider that resolves app slugs/client IDs or dynamic provider-wide app group. `most_specific_pattern_then_closest`, `prose_tokens_exact_actor_directory`. |
| Grants 8: manage scope and global listing | Per-pattern manage for old and new scopes, global manage to list, own-only explanation without global manage. `scoped_manage_checks_reach`, `scoped_cli_and_actual_loopback`. Minting credentials remains OS-owner only. |
| Grants 9, 10: narrowing and admission ceilings | Check symbolic region boundaries, own policy under an everyone lock and effective caps in the same transaction. A manager cannot introduce subjects in a way that would give them broader everyone rights. `scoped_manage_checks_reach`. |
| Grants 11, 12: self-lockout and atomic batches | Check authority after simulated mutations, including the combined effect of a lock plus a grant keeping its maker in. Opt in to irreversible delegated lockout explicitly; OS owner can always recover. Keyed batches roll back their grants and events on failure. `scoped_manage_checks_reach`, `inherited_everyone_lock`, `grant_batches_are_atomic`. |
| Grants 13: member delegates under an everyone lock | The reference kit marks this case skipped. Local positive test: `inherited_everyone_lock`; delegated new member remains locked while the manager's own broader authority suffices. |
| Grants 14: cross-team project/environments | **Not present:** one OS-administered board; no provider team/project identity hierarchy. The reference kit also skips this case; that is not evidence of local equivalence. |

## Schemas and files

| Reference case | Local implementation and evidence |
| --- | --- |
| Schemas 1: built-ins first, get by ID | Built-in `git-commit@1` plus vendor-neutral HTTP(S) `deployment@1`, then custom versions. Unknown ID is distinct from invalid ID. `local_deployment_schema_validates`, `a_v1_deployment_schema_keeps`, `source_filter_selects_inbound_origin`. Hosted deployment identity/host validation is not copied. |
| Schemas 2, 3: immutable registration, allowed definition subset | Canonical parsed JSON compare includes description; immutable versions, bounded definitions and registry, reserved built-ins, no external refs. [Schema contract tests](../crates/board-core/tests/schema_contract.rs), `channel_and_schema_retries`. |
| Schemas 4: typed data fence | Validate bounded structural keywords and supported formats **before commit** on post and edit. Malformed or unknown fences do not overwrite earlier content. `local_blocks_are_immutable`, `checked_data_fences_include_html`, `schema_value_formats_and_numeric_steps`. |
| Schemas 5: read vs global manage | Active delegates can inspect definitions. Registration requires effective manage at the wildcard pattern; regional authority does not qualify. `inherited_everyone_lock`. |
| Attachments 1, 2: upload/post/read, dotfiles | Raw bytes remain in SQLite, IDs reusable; origin and live-placement permissions checked on reads/reuse. Reject hidden names, paths and invisible controls. Resumable offsets and digest verification are local alternatives to external upload URLs. [Upload tests](../crates/board-core/tests/uploads.rs), `message_workflow_profiles`. |
| Attachments 3: task roots carry files | Same message files and stale retry semantics as ordinary posts. [task_attachments_are_message_files](../crates/board-api/src/work/tests.rs). |

Run without network after dependencies are fetched:

```sh
cargo test --offline --locked -p board-local -p board-api -p board-cli
cargo build --offline --locked -p board-cli
node scripts/compare-reference.mjs /path/to/reference target/debug/board
node scripts/compare-schema-values.mjs /path/to/reference target/debug/board
node scripts/compare-reactions.mjs /path/to/reference target/debug/board
```

The Node scripts are developer-only checks using the reference as a read-only
oracle and temporary databases. They compare selected shape, fence, mention and
schema value cases through real Rust CLI processes. Agreement on a finite corpus
is not exhaustive format equivalence, wire compatibility, or a hosted integration test.
