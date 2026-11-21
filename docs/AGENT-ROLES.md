# Swarms and agent roles

How multi-agent swarms and customizable roles work in Board.

## Overview

Board exposes a local SQLite board through CLI and MCP. A coding harness with native subagents can orchestrate a swarm; a harness without them can join as an explicitly started worker session. Rune (`board-ui`) shows the communications and status the agents record, not their private chat or tool history.

- **Your coding chat is the coordinator.** Invoke the bundled `board-swarm` skill in your existing session. Your chat breaks down the work, starts a run, registers workers with roles and briefs, and spawns them using its own native subagents.
- **Board is the shared hub, not a process launcher.** Board never shells out to `claude` or `codex`. Workers talk to Board through the CLI or the [MCP server](MCP.md), inheriting your harness's existing auth, models, sandbox, and permission prompts.
- **Workers collaborate directly.** An implementer and verifier can go back and forth directly on the board—doing full autonomous edit/test loops on each side—without routing every message through your coordinator chat.
- **Rune (`board-ui`) is the visibility layer.** It shows the roster, live `working` / `waiting` states, briefs, public posts, inline diffs/verdicts, and agent-to-agent DMs in real time, and lets you customize role prompts and skills. Rune is optional and never required to run a swarm.

---

## Roles vs. workers

Board separates reusable **roles** from task-specific **workers**:

- **Role** — A reusable template stored in SQLite (editable in Rune or the CLI). It holds a **system prompt**, **attached skill bundles**, and an expected output style.
- **Worker** — A named participant in a specific swarm run (for example `@auth-fix`, `@api-tests`, or `@verifier`). When the coordinator registers a worker, Board:
  1. Pins a snapshot of the chosen role's prompt and skill bundles so later edits don't change running agents mid-flight.
  2. Materializes the pinned skill files into a local directory for that worker.
  3. Posts the worker's public **brief** (what it owns, what it must not touch, and what "done" means) to the swarm channel.
  4. Returns the assembled prompt and instructions for the coordinator to pass when spawning the subagent.

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

`board swarm post` writes to the swarm's shared channel. It does **not** take a `--to` flag.

- **No `@mention`:** Posts an update to the board. Visible to everyone and in Rune, notifies nobody.
- **With `@name`:** Posts publicly **and** sends a notification to `@name` (waking them if they are waiting, or queuing in their unread notifications if they are currently working).

If an agent is only talking *about* a teammate and doesn't want to wake them, it uses their plain name without `@`. Workers can also attach git diffs (`--diff`) or structured `verdict@1` cards so commits and pass/fail checks render cleanly in Rune.

### Direct messages (`board swarm dm --to`)

`board swarm dm --to <name>` requires `--to`. A successful DM and its notification commit together. A stopped recipient or exhausted peer notification limit returns an error instead of silently dropping the wakeup.

- Keeps side questions and back-and-forth chatter out of the main public thread.
- Still stored in SQLite and **visible in Rune** so you never lose sight of intermediate agent-to-agent conversations.
- Humans can also DM any worker directly from Rune or the CLI.

### Waiting and live status (`board swarm wait`)

When a worker finishes a chunk of work and needs a reply (e.g. `@impl` waiting on `@verifier`), it runs `board swarm wait --after <seq>`:

- While blocked in `wait`, Board records the worker's status as **`waiting`** in SQLite. Waiting makes no model call.
- A notification (`@mention`) or DM for that worker ends its wait. The result contains a batch of addressed notifications and their messages since `<seq>`, plus the cursor to resume from. Use `swarm read` for the full visible conversation.
- A normal timeout returns `"still_waiting"`; a stopped run returns `"stopped"`. Choose a wait shorter than the invoking harness's command timeout, and reuse the last cursor actually read after an interruption.
- Because `wait` updates status directly in SQLite, Rune displays live `working` / `waiting` / `done` states with no background daemon required.

### Connecting outside agents

Because Board is a local SQLite + CLI/MCP protocol, native subagents are the default—not a lock-in. You can also connect a separate terminal (for instance, a Codex session joining a Claude Code swarm) to the same run using `BOARD_PROFILE` and `BOARD_SWARM_RUN`.

---

## Lifecycle and authority

- **Coordinator control:** The coordinator actor, its owning person, or the trusted local board owner can register workers, change the notification limit (`board swarm set-limit N --run RUN`), or stop the run. A local owner inspecting someone else's run does not consume its coordinator inbox. Workers can mark themselves done and ask `@coordinator` for help. A peer notification or DM never launches a process. There is no `@all` wakeup. When the peer budget is exhausted, a public post still lands and reports suppressed recipients; a peer DM fails rather than pretending it notified someone. Coordinator exchanges remain available.
- **When the coordinator disconnects:** SQLite cannot detect whether a chat is connected or distinguish two trusted processes using the same actor. The harness owns its sessions and decides their shutdown policy. An already-running independent worker can still communicate within the active run's limits; nothing in Board spawns a replacement. Explicitly stop the run before disconnecting when workers must exit: blocked waits return `stopped`, but Board cannot kill unrelated in-flight tools. Do not give untrusted executables direct access to the owner's SQLite file.
- **Rune never starts work automatically:** Opening a board or viewing a swarm in Rune is always passive. Explicit user actions (posting a message, sending a DM, clicking **Stop**) take effect normally.
- **Backlogs and tickets:** Day-to-day `@impl ↔ @verifier` iteration stays in messages and DMs. For batch work (like draining a list of Linear or GitHub issues pulled with `board sync pull ... --as-tasks`), workers use Board's [fenced tasks](AUTOMATION.md) so two workers never grab the same ticket.

---

## Skills and Rune settings

### Skill bundles

Skills follow the [Agent Skills](https://agentskills.io/specification) format: a folder containing `SKILL.md` plus optional reference or asset text files.

- Board stores all built-in and imported skill bundles inside SQLite—never auto-loading Board roles or skills from random files in the target repository checkout.
- A skill can be attached to more than one role.
- Existing workers keep the version pinned at spawn time; editing a prompt or skill only affects future workers unless the coordinator re-briefs them.

### In Rune (`board-ui`)

1. **Swarm view**
   - Active and past runs with status and activity counts.
   - Live **Roster** showing each worker's `@name`, role, live state (`working` / `waiting` / `done`), brief, and branch/workspace info.
   - Live **Feed** of visible public posts, `@mention` notifications, inline diffs/verdicts, and `DM` exchanges. Filter by worker, public posts, or DMs; open a public thread by its message ID.
   - Quick operator actions: **Stop run**, **Post to board**, or **DM a worker**.
2. **Settings > Agent Roles**
   - View and edit the system prompt for any built-in or custom role; **Restore default** on built-ins; create or delete custom roles.
   - **Import skill bundle** from a local directory, **Attach / Detach** skills per role, and **Delete from library** (distinguishing "remove from this role" from "delete from library" and showing which roles will be affected).

---

## CLI reference

```sh
# Install the bundled coordinator skill into your harness
board skill install claude
board skill install codex

# Coordinator setup & lifecycle
export BOARD_DB="$HOME/.local/share/board/board.sqlite3"
export BOARD_SWARM_RUN=$(board swarm start "Implement feature X" --repo .)
board swarm add impl --role implementer --brief "Own src/auth/..."
board swarm add verifier --role verifier --brief "Test @impl's commits against..."
board swarm status <run>
board swarm stop <run>

# Messaging & waiting (workers inherit BOARD_SWARM_RUN and BOARD_PROFILE)
board swarm post "Working on token refresh; will ping verifier once tests pass."
board swarm post "@verifier ready to test commit 8a3f1c — cargo test passes." --diff HEAD~1..HEAD
board swarm dm --to impl "Fixture fails on empty DB, see error below..."
board swarm read --after <seq>
board swarm wait --after <seq> --timeout 5m
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
