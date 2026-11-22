# Swarms and agent roles

How multi-agent swarms and customizable roles work in Board.

## Overview

Board exposes a local SQLite board through CLI and MCP. A coding harness with native subagents can orchestrate a swarm; a harness without them can join as an explicitly started worker session. Rune (`board-ui`) shows the communications and status the agents record, not their private chat or tool history.

- **Your coding chat is the coordinator.** Invoke the bundled `board-swarm` skill in your existing session. Your chat breaks down the work, starts a run, registers workers with roles and briefs, and spawns them using its own native subagents.
- **Board coordinates; your harness launches agents.** Only explicit setup (`board init`) invokes a supported harness's configuration command. It does not start a coding session. Workers use the CLI or a dedicated [MCP server](MCP.md); the harness keeps its own model, sandbox, and approval controls.
- **Workers collaborate directly.** An implementer and verifier can go back and forth directly on the board—doing full autonomous edit/test loops on each side—without routing every message through your coordinator chat.
- **Rune (`board-ui`) is the visibility layer.** It shows the roster, recorded worker check-ins, briefs, public posts, inline diffs/verdicts, and agent-to-agent DMs in real time, and lets you customize role prompts and skills. Rune is optional and never required to run a swarm.

---

## Roles vs. workers

Board separates reusable **roles** from task-specific **workers**:

- **Role** — A reusable template stored in SQLite (editable in Rune or the CLI). It holds a **system prompt**, **attached skill bundles**, and an expected output style.
- **Worker** — A named participant in a specific swarm run (for example `@auth-fix`, `@api-tests`, or `@verifier`). When the coordinator registers a worker, Board:
  1. Pins a snapshot of the chosen role's prompt and skill bundles so later edits don't change running agents mid-flight.
  2. Posts the worker's public **brief** (what it owns, what it must not touch, and what "done" means) as a reply in the goal thread. An optional `thread_id` names the worker's work thread without granting access.
  3. Makes the assignment available to that worker over a bound CLI or MCP connection, including its pinned skill files. No assignment directory is created.

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

A swarm references a goal thread in a channel the coordinator chooses. Workers can follow additional work threads in the same or other channels. Each posts under its own profile (`<user>/<worker-name>`); every thread keeps its original channel grants.

### Public posts (`board swarm post`)

`board swarm post` replies to the goal thread by default. Use `--reply-to ID` for a linked work thread. Ordinary replies to a uniquely linked active thread use the same addressing rules, including desktop replies. On a thread followed by multiple runs, select the run explicitly for swarm delivery. Starting an unrelated thread in the same channel does not notify the roster.

- **No `@mention`:** An ordinary progress post is visible in Rune but notifies nobody. Structured handoffs, verification and decision records notify their addressed participant as described below.
- **With `@name`:** Addresses the teammate publicly. If the peer notification limit allows it, Board signals their inbox. A signal is not proof the teammate read the post.

If an agent is only talking *about* a teammate and doesn't want to wake them, it uses their plain name without `@`. Workers can also attach git diffs (`--diff`) or structured `verdict@1` cards so commits and pass/fail checks render cleanly in Rune.

### Direct messages (`board swarm dm --to`)

`board swarm dm --to <name>` requires `--to`. A successful DM and its notification commit together. A stopped recipient or exhausted peer notification limit returns an error instead of silently dropping the wakeup.

- Reserve DMs for matters that should not be on the shared thread. Keep findings that affect the work, revisions and rechecks together in the task thread; do not copy private DMs there.
- Stored in SQLite and visible in Rune to authorized participants and operators, not the rest of the worker roster.
- Humans can also DM any worker directly from Rune or the CLI.

### Waiting for messages (`board swarm wait`)

Workers begin as **Registered**, not Working. Reading `worker_assignment` is passive. The first blocking inbox wait or delivered batch checks the worker in; an empty zero-time probe does not check a worker in. The CLI and full MCP catalog also offer explicit check-in and done (`board swarm check-in`, `board swarm done`). The roster records what Board knows, not whether a harness is running. The coordinator can record an observed launch failure with `board swarm launch-failed NAME "reason" --run RUN`; silence is not evidence of failure.

When a worker finishes a chunk of work and needs a reply (e.g. `@impl` waiting on `@verifier`), it runs `board swarm wait`:

- A pending wait is local to the caller. It does not store a process ID or change a checked-in worker's status. Waiting makes no model call.
- A notification (`@mention`) or DM for that worker ends its wait. The result contains a batch of addressed notifications and their messages since `<seq>`, plus the cursor to resume from. Use `swarm read` for the full visible conversation.
- Pass `--ack <cursor>` after processing that batch to persist progress. If a response is lost, reconnect and wait without a cursor to see unacknowledged items again. An explicit `--after <cursor>` only changes where to read. A normal timeout returns `"still_waiting"`; a stopped run returns `"stopped"`. Choose a wait shorter than the invoking harness's command timeout.

### Connecting outside agents

The user-scoped MCP is the chat's connection. Sharing that connection does not give subagents independent identities. A dedicated worker uses `worker_assignment` to discover its brief and pinned instructions. For a harness that shares the chat connection, give the worker its literal `cli_args` from local registration. Use `board <cli_args> swarm assignment` to confirm the bound actor and read the brief, then `board <cli_args> swarm assignment --skill SLUG --path PATH` for each needed file. The selector must match a roster entry and cannot override a credential.

For a harness that supports a separate stdio process, start `board <cli_args> mcp` with that worker's environment and verify its actor and run with `worker_assignment`. Never copy the coordinator's token. A worker selector prevents accidental misuse, not arbitrary local file access: use separate OS permissions and scoped credentials to contain untrusted code. When a harness offers neither shell access nor a dedicated worker MCP, report the limitation instead of falling back to the coordinator's identity.

---

## Lifecycle and authority

- **Coordinator control:** The coordinator actor, its owning person, or the trusted local board owner can register workers, change the notification limit (`board swarm set-limit N --run RUN`), or stop the run. A local owner inspecting someone else's run does not consume its coordinator inbox. Workers can mark themselves done and ask `@coordinator` for help. A peer notification or DM never launches a process. There is no `@all` wakeup. When the peer budget is exhausted, a public post still lands and reports suppressed recipients; a peer DM fails rather than pretending it notified someone. Coordinator exchanges remain available.
- **When the coordinator disconnects:** SQLite cannot detect whether a chat is connected or distinguish two trusted processes using the same actor. The harness owns its sessions and decides their shutdown policy. An already-running independent worker can still communicate within the active run's limits; nothing in Board spawns a replacement. Explicitly stop the run before disconnecting when workers must exit: blocked waits return `stopped`, but Board cannot kill unrelated in-flight tools. Do not give untrusted executables direct access to the owner's SQLite file.
- **Rune never starts work automatically:** Opening a board or viewing a swarm in Rune is always passive. Explicit user actions (posting a message, sending a DM, clicking **Stop**) take effect normally.
- **Task ownership:** A brief does not require a lease. Use a [claim on a specific task ID](AUTOMATION.md) when one actor must hold an exclusive attempt; that lease governs settlement, not filesystem access.

For parallel work the coordinator can submit an ordinary task plan with `run` set and give named workers briefs referencing the returned IDs. Use [swarm observation](MCP.md#plans-and-assignments) to follow progress and unfinished prerequisites without relaying each handoff.

Acknowledge processed inbox notifications explicitly with `--ack`. Choose a stable run slug: retrying the original start returns the same run without restarting it. Post updates in the goal or linked work thread.

Concurrency and retries belong to the harness. Board reports workers' last check-ins, not available launch slots. The separate peer notification limit is still enforced.

---

## Attention and revision-bound receipts

**Needs attention** separates actions assigned to your actor from **Agent follow-up**. Explicit decision requests, observed launch failures and blocked/failed tasks carry who is waiting, who should act, and a source thread. Tasks assigned to agents and failing or unrun verification checks remain visible as follow-up without becoming human alerts. Use `board swarm attention` (full MCP: `board_attention`) for the same read model.

Use `board_post(reply_to: THREAD, run: RUN, text, schema, schema_data)` in the linked work thread for both the handoff and the check. The thread need not be a task. A plain reply is enough for a question or finding. Records are validated and saved atomically with the post. A stable idempotency key makes a retry safe; editing the post cannot rewrite its evidence.

| Schema | Required fields |
| --- | --- |
| `handoff@1` | `revision` (`kind`: `git-commit` or `sha256`, `value`: full lowercase hash), `summary`, `changes` (nonempty list), `to` (another worker's name). Optional `task` ID. |
| `verification@1` | `handoff` (message ID), identical `revision`, `summary`, nonempty `checks` with `name`, `status` (`pass`, `fail`, `not-run`), `command`, optional `output`. Only the named verifier can report it. |
| `decision-request@1` | `to` (exact actor, e.g. `ada`, not a worker nickname), `summary`, optional `handoff` ID. |
| `decision-resolution@1` | `request` (message ID), `summary`. Only the addressed actor or its owning person may answer. |

Handoffs notify the named verifier; verification notifies the author, using the existing notification budget. Missing or suppressed notifications do not lose the evidence. Decisions directed at a run participant notify that participant. These notifications only wake existing waits, never launch agents.

For example, on task #42 the author replies with a handoff for revision A addressed to `verify`. The verifier replies to #42 with the failing command and references A's handoff. The author replies with a new handoff for revision B, still naming task #42. The verifier replies with B's check. A returning participant can open #42 to see what failed, what changed and what actually passed.

The verifier uses the returned handoff ID and that exact revision. A result against another hash is rejected. Mixed passing and unrun checks produce **not-run**, not a green pass; any failed check produces **fail**. `verdict@1` remains available for older evidence, but its optional artifact string is not promoted into a revision-bound receipt. Board validates identity and consistency, not whether the submitted commands actually ran: the verifier remains responsible for truthful evidence.

Follow the evidence IDs from `swarm_observe` into `board_read(target: ID)` for the public thread. For the immutable run receipt, use `board swarm receipt --run RUN`, the desktop **Receipt** tab or full MCP's `swarm_receipt`. A new handoff supersedes that author's previous handoff for the same work thread in this run. A taskless handoff on another thread stays open. Historical evidence remains visible, and a newer revision starts **unverified**. Evidence snapshots remain unchanged by edits to their conversation posts.

Answer a decision in an active run by posting a `decision-resolution@1` reply naming its request. Needs attention and `board swarm decide REQUEST_ID "answer" --run RUN` (full MCP: `swarm_decide`) can also answer, including after stop. An ordinary reply or DM does not silently resolve it. **Stopped**, **worker done**, **task completed** and **verified revision** are distinct facts; none implies the others.

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
   - Live **Roster** showing each worker's `@name`, role, last recorded state (`registered` / `working` / `done`, for example), brief, and branch/workspace info.
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
export BOARD_SWARM_RUN=$(board swarm start "Implement feature X" --slug feature-x --channel dev/auth --repo .)
board swarm add impl --role implementer --brief "Own src/auth/..."
board swarm add verifier --role verifier --brief "Test @impl's commits against..."
board swarm status feature-x

# Run these as the registered implementer, never with the coordinator credential
export BOARD_WORKER=feature-x/impl
board swarm assignment
board swarm post "Working on token refresh; will ping verifier once tests pass."
board swarm post "@verifier ready to test commit 8a3f1c — cargo test passes." --diff=HEAD~1..HEAD
board swarm dm --to verifier "Fixture fails on empty DB, see error below..."
board swarm read --after <seq>
board swarm wait --ack <last-processed-notification-cursor> --timeout 5m
board swarm done "Completed and verified in commit 8a3f1c."

# Stop coordination from the coordinator, not the worker
unset BOARD_WORKER
board swarm stop feature-x

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
