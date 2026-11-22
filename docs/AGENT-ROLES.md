# Swarms and agent roles

How multi-agent swarms and customizable roles work in Board.

## Overview

Board exposes a local SQLite board through CLI and MCP. A coding harness with native subagents can orchestrate a swarm; a harness without them can join as an explicitly started worker session. Rune (`board-ui`) shows the communications and status the agents record, not their private chat or tool history.

- **Your coding chat is the coordinator.** Invoke the bundled `board-swarm` skill in your existing session. Your chat breaks down the work, starts a run, registers workers with roles and briefs, and spawns them using its own native subagents.
- **Board coordinates; your harness launches agents.** Only explicit setup (`board init`) invokes a supported harness's configuration command. It does not start a coding session. Workers use the CLI or a dedicated [MCP server](MCP.md); the harness keeps its own model, sandbox, and approval controls.
- **Workers collaborate directly.** An implementer and verifier can go back and forth directly on the board—doing full autonomous edit/test loops on each side—without routing every message through your coordinator chat.
- **Rune (`board-ui`) is the visibility layer.** It shows the roster, live `working` / `waiting` states, briefs, public posts, inline diffs/verdicts, and agent-to-agent DMs in real time, and lets you customize role prompts and skills. Rune is optional and never required to run a swarm.

---

## Roles vs. workers

Board separates reusable **roles** from task-specific **workers**:

- **Role** — A reusable template stored in SQLite (editable in Rune or the CLI). It holds a **system prompt**, **attached skill bundles**, and an expected output style.
- **Worker** — A named participant in a specific swarm run (for example `@auth-fix`, `@api-tests`, or `@verifier`). When the coordinator registers a worker, Board:
  1. Pins a snapshot of the chosen role's prompt and skill bundles so later edits don't change running agents mid-flight.
  2. Posts the worker's public **brief** (what it owns, what it must not touch, and what "done" means) to the swarm channel.
  3. Makes the assignment available to that worker over MCP, including its pinned skill files. A local prompt and skill directory are an optional export, not a prerequisite for participating.

### Built-in roles

Board ships with six built-in roles. Each has a default prompt and an attached instruction-only skill. Edit the role or replace its attached skills in Rune; **Restore default** restores the built-in role's name, description, and prompt, not its skill attachments.

| Role | Responsibility | Typical output on the board |
| --- | --- | --- |
| `implementer` | Build or fix a scoped slice of code | Git commit/diff reference, summary of changes, tests run, known limitations |
| `verifier` | Independently test and validate against acceptance criteria | `pass`, `fail`, or `not-run` verdict citing the exact commit, commands run, and output/evidence |
| `reviewer` | Critique design, edge cases, and maintainability without editing the patch | Findings with severity and file/line locations, or approval + residual risk |
| `architect` | Decompose ambiguous problems and define interfaces before coding | Proposal naming alternatives, tradeoffs, affected files, and worker slices |
| `documenter` | Write or update docs and examples matching real behavior | Scoped doc changes plus proof that commands/examples work |
| `designer` | Propose or refine UI/UX flows, layouts, states, and visual details | Inspectable spec, layout/UI changes, states, and screenshots/captures |

Every worker also receives a short **Board Swarm Protocol** teaching it to:
- stay inside its brief's ownership boundaries,
- work autonomously (edit, run tests, iterate) before posting concise updates referencing message numbers (`#42`) and git commits,
- only `@mention` someone when it actually wants to notify/wake them,
- wait cleanly with `board swarm wait` instead of polling in a tight loop.

### Roles are not a security boundary

Assigning a role is guidance, not OS sandboxing or permission elevation. Each harness enforces its own tool and approval controls, and Board's [delegated credentials and channel grants](ACCESS.md) stay separate. If a worker lacks a tool needed to verify something, it reports that check as *not run / blocked* instead of pretending it passed.

---

## Messaging, notifications, and DMs

Every swarm run gets a Board channel (`swarms/<run-slug>`), and each worker posts under its own profile (`<user>/<worker-name>`).

### Public posts (`board swarm post`)

`board swarm post` writes to the swarm's shared channel. It does **not** take a `--to` flag. Ordinary posts and thread replies in an active swarm channel use the same addressing rules, including replies from the desktop task thread.

- **No `@mention`:** An ordinary progress post is visible in Rune but notifies nobody. Structured handoffs, verification and decision records notify their addressed participant as described below.
- **With `@name`:** Addresses the teammate publicly. If the peer notification limit allows it, Board signals their inbox. A signal is not proof the teammate read the post.

If an agent is only talking *about* a teammate and doesn't want to wake them, it uses their plain name without `@`. Workers can also attach git diffs (`--diff`) or structured `verdict@1` cards so commits and pass/fail checks render cleanly in Rune.

### Direct messages (`board swarm dm --to`)

`board swarm dm --to <name>` requires `--to`. A successful DM and its notification commit together. A stopped recipient or exhausted peer notification limit returns an error instead of silently dropping the wakeup.

- Reserve DMs for matters that should not be on the shared thread. Keep findings that affect the work, revisions and rechecks together in the task thread; do not copy private DMs there.
- Stored in SQLite and visible in Rune to authorized participants and operators, not the rest of the worker roster.
- Humans can also DM any worker directly from Rune or the CLI.

### Waiting and live status (`board swarm wait`)

Workers begin as **Registered**, not Working. After checking its assigned identity, a worker explicitly runs `board swarm check-in` (MCP: `swarm_check_in`). Entering `swarm wait` also records an explicit worker check-in. The roster shows the last recorded swarm activity separately from status: a quiet worker is not automatically marked failed. The coordinator reports an actual harness launch error with `board swarm launch-failed NAME "reason" --run RUN`; only a worker that has not checked in can have a launch failure recorded. A later successful check-in clears that error.

Schema v17 preserves existing assignments and history. Old Working records return to Registered because the old registration never proved execution; old Waiting records retain the explicit wait registration. Take a backup with the old binary before upgrading. This is not an assertion that an existing harness process stopped.

When a worker finishes a chunk of work and needs a reply (e.g. `@impl` waiting on `@verifier`), it runs `board swarm wait`:

- While blocked in `wait`, Board records the worker's status as **`waiting`** in SQLite. Waiting makes no model call.
- A notification (`@mention`) or DM for that worker ends its wait. The result contains a batch of addressed notifications and their messages since `<seq>`, plus the cursor to resume from. Use `swarm read` for the full visible conversation.
- Pass `--ack <cursor>` after processing that batch to persist progress. If a response is lost, reconnect and wait without a cursor to see unacknowledged items again. An explicit `--after <cursor>` only changes where to read. A normal timeout returns `"still_waiting"`; a stopped run returns `"stopped"`. Choose a wait shorter than the invoking harness's command timeout.
- Because `wait` updates status directly in SQLite, Rune displays live `working` / `waiting` / `done` states with no background daemon required.

### Connecting outside agents

The user-scoped MCP is the chat's connection. Sharing that connection does not give subagents independent identities. A dedicated worker uses `worker_assignment` to discover its brief and pinned instructions. For a harness that shares the chat connection, request a local package with MCP `swarm_add(export_local: true)` or CLI `board swarm add` and give the worker the returned prompt, files, environment and literal CLI arguments. Use `board swarm add --portable` for registration without local files. On a shared harness, **use the CLI**, for example `board --db /absolute/board.sqlite3 --worker release/check --json whoami`. The selector must match a roster entry and cannot override a credential.

For a harness that genuinely supports a separate stdio process, start `board <cli_args> mcp` with that worker's environment and verify its `board_whoami` result. Never copy the coordinator's token. A worker selector prevents accidental misuse, not arbitrary local file access: use separate OS permissions and scoped credentials to contain untrusted code. When a harness offers neither shell access nor a dedicated worker MCP, report the limitation instead of falling back to the coordinator's identity.

---

## Lifecycle and authority

- **Coordinator control:** The coordinator actor, its owning person, or the trusted local board owner can register workers, change the notification limit (`board swarm set-limit N --run RUN`), or stop the run. A local owner inspecting someone else's run does not consume its coordinator inbox. Workers can mark themselves done and ask `@coordinator` for help. A peer notification or DM never launches a process. There is no `@all` wakeup. When the peer budget is exhausted, a public post still lands and reports suppressed recipients; a peer DM fails rather than pretending it notified someone. Coordinator exchanges remain available.
- **When the coordinator disconnects:** SQLite cannot detect whether a chat is connected or distinguish two trusted processes using the same actor. The harness owns its sessions and decides their shutdown policy. An already-running independent worker can still communicate within the active run's limits; nothing in Board spawns a replacement. Explicitly stop the run before disconnecting when workers must exit: blocked waits return `stopped`, but Board cannot kill unrelated in-flight tools. Do not give untrusted executables direct access to the owner's SQLite file.
- **Rune never starts work automatically:** Opening a board or viewing a swarm in Rune is always passive. Explicit user actions (posting a message, sending a DM, clicking **Stop**) take effect normally.
- **Backlogs and tickets:** Workers use Board's [fenced tasks](AUTOMATION.md) so two workers never grab the same ticket. Review happens in the task thread, without a coordinator relaying messages.

For new parallel work, the coordinator can submit an ordinary task plan to the swarm channel, launch a small worker pool in the chat, and inspect counts and exceptions with `swarm observe` instead of relaying each handoff. The [pool workflow](MCP.md#pool-workflow) covers dependency fan-in, queue waits, renewal, and coordinator reconnects. Schema v19 removes the separate swarm plan ledger without changing existing tasks, batch receipts, assignments, run policy or role attachments. Back up with the old binary before upgrading.

Schema v20 adds optional worker runtime reports and viewer-specific public-feed bookmarks. Existing assignments, tasks and inbox positions are preserved. Update workers that previously relied on an implicit acknowledgment: after processing a batch, send its returned notification cursor with `ack` (CLI: `--ack`). Clients that already supply `after` on each read can still page explicitly. After installing the new binary, rerun `board init` to upgrade an unmodified stock coordinator skill.

---

## Attention and revision-bound receipts

**Needs attention** separates actions assigned to your actor from **Agent follow-up**. Explicit decision requests, observed launch failures and blocked/failed tasks carry who is waiting, who should act, and a source thread. Tasks assigned to agents and failing or unrun verification checks remain visible as follow-up without becoming human alerts. Use `board swarm attention` (MCP: `board_attention`) for the same read model.

For a task in `swarms/RUN`, use `board_reply(id: TASK, text, schema, schema_data)` or `board reply TASK --schema NAME@1 'JSON'`. Both workers reply to the same task ID. A plain reply is enough for a question or finding; use the records below for an independently checkable result. For a task outside the run channel, keep structured swarm evidence in the run channel and link the task rather than moving its discussion. Records are validated and saved atomically with the post. A stable idempotency key makes a retry safe; editing the post cannot rewrite its evidence.

| Schema | Required fields |
| --- | --- |
| `handoff@1` | `revision` (`kind`: `git-commit` or `sha256`, `value`: full lowercase hash), `summary`, `changes` (nonempty list), `to` (another worker's name). Optional `task` ID. |
| `verification@1` | `handoff` (message ID), identical `revision`, `summary`, nonempty `checks` with `name`, `status` (`pass`, `fail`, `not-run`), `command`, optional `output`. Only the named verifier can report it. |
| `decision-request@1` | `to` (exact actor, e.g. `ada`, not a worker nickname), `summary`, optional `handoff` ID. |
| `decision-resolution@1` | `request` (message ID), `summary`. Only the addressed actor or its owning person may answer. |

Handoffs notify the named verifier; verification notifies the author, using the existing notification budget. Missing or suppressed notifications do not lose the evidence. Decisions directed at a run participant notify that participant. These notifications only wake existing waits, never launch agents.

For example, on task #42 the author replies with a handoff for revision A addressed to `verify`. The verifier replies to #42 with the failing command and references A's handoff. The author replies with a new handoff for revision B, still naming task #42. The verifier replies with B's check. A returning participant can open #42 to see what failed, what changed and what actually passed.

The verifier uses the returned handoff ID and that exact revision. A result against another hash is rejected. Mixed passing and unrun checks produce **not-run**, not a green pass; any failed check produces **fail**. `verdict@1` remains available for older evidence, but its optional artifact string is not promoted into a revision-bound receipt. Board validates identity and consistency, not whether the submitted commands actually ran: the verifier remains responsible for truthful evidence.

Read `board swarm receipt --run RUN` (MCP: `swarm_receipt`) or the run's **Receipt** tab to see changes, full revisions, verifier identity, commands/results, linked unfinished tasks and unresolved decisions. A new handoff by the same author for the same task supersedes the previous one; without a task ID, it supersedes that author's previous unlinked handoff. Historical evidence remains visible, and a newer revision starts **unverified**. Evidence snapshots remain unchanged by edits to their conversation posts.

Answer a decision in Needs attention, or run `board swarm decide REQUEST_ID "answer" --run RUN` (MCP: `swarm_decide`). This records a resolution and clears the request, including after a run stops. An ordinary reply or DM does not silently resolve it. **Stopped**, **worker done**, **task completed** and **verified revision** are distinct facts; none implies the others.

---

## Skills and Rune settings

### Skill bundles

Skills follow the [Agent Skills](https://agentskills.io/specification) format: a folder containing `SKILL.md` plus optional reference or asset text files.

- Board stores all built-in and imported skill bundles inside SQLite—never auto-loading Board roles or skills from random files in the target repository checkout.
- A skill can be attached to more than one role.
- Existing workers keep the version pinned at spawn time; editing a prompt or skill only affects future workers unless the coordinator re-briefs them.

### In Rune (`board-ui`)

1. **Swarm view**
   - Active and past runs with the goal, status and worker count.
   - Live **Roster** showing each worker's `@name`, role, live state (`working` / `waiting` / `done`), brief, and branch/workspace info.
   - Live **Feed** of visible public posts, `@mention` notifications, inline diffs/verdicts, and `DM` exchanges. Filter by worker, public posts, or DMs; open a public thread by its message ID.
   - Quick operator actions: **Stop run**, **Post to board**, or **DM a worker**.
2. **Settings > Agent Roles**
   - View and edit the system prompt for any built-in or custom role; **Restore default** on built-ins; create or delete custom roles.
   - **Import skill bundle** from a local directory, **Attach / Detach** skills per role, and **Delete from library** (distinguishing "remove from this role" from "delete from library" and showing which roles will be affected).

---

## CLI reference

```sh
# Register MCP and install the coordinator skill in detected harnesses
board init

# Coordinator setup & lifecycle
export BOARD_DB="$HOME/.local/share/board/board.sqlite3"
export BOARD_SWARM_RUN=$(board swarm start "Implement feature X" --repo .)
board swarm add impl --role implementer --brief "Own src/auth/..."
board swarm add verifier --role verifier --brief "Test @impl's commits against..."
board swarm status <run>
board swarm stop <run>

# Worker commands below assume the entire returned worker environment, including BOARD_WORKER
board swarm post "Working on token refresh; will ping verifier once tests pass."
board swarm post "@verifier ready to test commit 8a3f1c — cargo test passes." --diff=HEAD~1..HEAD
board swarm dm --to impl "Fixture fails on empty DB, see error below..."
board swarm read --after <seq>
board swarm wait --ack <last-processed-notification-cursor> --timeout 5m
board swarm done "Completed and verified in commit 8a3f1c."

# Roles & skill bundles (CLI parity with Rune Settings)
board roles list
board roles show implementer
board roles edit implementer --prompt-file prompt.md
board roles reset implementer
board skills import ./my-skill-dir
board roles attach-skill implementer my-skill
board roles detach-skill implementer my-skill
board skills delete my-skill
```
