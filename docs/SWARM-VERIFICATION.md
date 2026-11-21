# Swarm verification

## Two native agents, one local board

A Codex chat registered two native subagents on a disposable SQLite board. The installed `board-swarm` skill came from the built CLI, using a temporary Codex skills directory. Each agent received the prompt, skill directory and environment returned by its own `board swarm add`. Board did not launch either agent. The sample was a Rust function that should return the last Unicode-whitespace-delimited field without allocation.

| Board cursor | Sender | What the other end observed |
| --- | --- | --- |
| 1, 2, 3 | Coordinator | The goal and two public briefs. No assignment notification was necessary to start a harness-created agent. |
| 4 | `verify` to `impl` (DM) | Five independent baseline cases: two passed, three Unicode cases failed. The implementer received the queued DM when it began its wait. |
| 8 | `impl`, mentioning `@verify` | The verifier received the exact file digest after the fix. An interrupted wait could be retried from the previous cursor. |
| 11 | `verify` (public, no mention) | A validated `verdict@1` recorded the digest, exact commands and results, with both test logs attached. All five independent cases and three implementation tests passed. |
| 12 | `verify` to `impl` (DM) | The implementer received the passing verdict directly. Both workers marked themselves done; no coordinator message relayed the exchange. |
| 18 | Coordinator | `swarm stop` closed the run; a fresh worker wait returned `stopped`. |

Rune rendered the baseline DM and waiting verifier during the run. A new Rune process opened an online backup after stop and showed both workers done, the handoff, and the structured verdict. The tested `fields.rs` SHA-256 was `0b04f613ba611227b9d8f59c71e116f99b8bb39b290aacfb7fa4f72fd449e0a2`.

![Rune reading the stopped run and independently verified verdict from the backup](swarm-verified.png)

## Repeatable checks

With build dependencies cached, no provider account, model API, or network service is required for these checks:

```sh
cargo fmt --all -- --check
cargo clippy --locked --offline --workspace --all-targets -- -D warnings
cargo test --locked --offline --workspace
cargo test --locked --offline -p board-cli --test processes \
  expired_process_cannot_finish_after_a_real_time_takeover -- --ignored --exact
cargo build --locked --offline --workspace --bins
target/debug/board --json conformance
python3 scripts/roundtrip.py --bin-dir target/debug
```

The round trip uses loopback: CLI post and duplicate retry, REST read and reply, an already-open Rune window, fresh processes, and restore from backup. Workspace tests cover v14/v15 upgrades, recovery of retired schedules without dispatch, explicit review for old composer drafts, watcher redelivery, immutable assignments, notification limits and cursor resume, scoped DMs, fenced claims, replay-safe ticket imports, real MCP pipes and Rune input/rendered pixels. See [Swarms and roles](AGENT-ROLES.md) and [MCP](MCP.md).

The CLI integration tests `init`, `init_upgrade` and `worker_identity` exercise setup without touching a real user profile, and run a global coordinator MCP alongside two differently bound workers on the same SQLite database. In a separate offline smoke test, the installed Codex and Claude Code executables were detected and registered in disposable homes. `codex mcp get board --json` reported enabled stdio; `claude mcp get board` reported a connected user-scope server. This tested the harness commands and the Board handshake, not a paid model turn. For the packaging test matrix and release prerequisites see [Install Board](INSTALL.md).

As a separate opt-in check, the CLI read public GitHub issue `octocat/Hello-World#1` without credentials or publishing enabled. `sync pull example 1 --as-tasks` queued 4,116 records and created one local task. We deliberately interrupted the long comment drain; a fresh offline `sync drain example --limit 1` applied the next record with no failures. This was a live read and restart test, not an assertion that all 4,116 records were applied. Slack and Linear were tested against local protocol fixtures, not live accounts.

The native-agent run and Rune captures were verified on Linux. The Claude install target and stdio protocol are tested without a paid session; this record does not claim a live Claude session, a macOS or Windows UI run, or any ability to continue model execution after the host harness exits.
