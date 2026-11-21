# Implementation plan

Detailed engineering blueprint for implementing the architecture documented in [AGENT-ROLES.md](AGENT-ROLES.md), [MCP.md](MCP.md), [SYNC.md](SYNC.md), and [ui/README.md](../ui/README.md).

Execute workstreams in order (1 → 6). Each workstream ends with concrete verification criteria and keeps `cargo test --workspace` green.

---

## Workstream 1: Remove Rhai middleware and legacy parity tracking

### 1.1 Remove Rhai write rules and source renderers

1. **Dependencies (`crates/board-core/Cargo.toml`)**:
   - Remove the `rhai` dependency and regenerate `Cargo.lock` (`cargo check --workspace`).
2. **Core (`crates/board-core/src/`)**:
   - Delete `crates/board-core/src/policy.rs`.
   - Remove `pub mod policy;` from `crates/board-core/src/lib.rs`.
   - Remove `crate::policy::init(&tx)?;` and all rule-evaluation / emitted-message / policy-drop hooks from `crates/board-core/src/core.rs`, `crates/board-core/src/automation/`, and `crates/board-core/src/sources/`.
   - Delete `crates/board-core/src/sources/render.rs` and remove `mod render;` and `source_renderers` lookups from `crates/board-core/src/sources/mod.rs` and `crates/board-core/src/sources/queue.rs` (outbound messages render directly from message text + configured author byline / file notices).
   - In the v15 SQLite migration (`core.rs`), drop legacy tables safely so older databases upgrade cleanly:
     ```sql
     DROP TABLE IF EXISTS write_rules;
     DROP TABLE IF EXISTS rule_internal;
     DROP TABLE IF EXISTS source_renderers;
     ```
   - Delete `crates/board-core/tests/write_rules.rs` and remove any Rhai renderer test cases from `crates/board-core/tests/sync.rs` and `crates/board-sync/src/tests/`.
3. **REST API (`crates/board-api/src/`)**:
   - Delete `crates/board-api/src/policy_api.rs`.
   - Remove `mod policy_api;`, `/v1/rules`, `/v1/rules/{name}`, and `/v1/sources/{name}/renderer` routes from `crates/board-api/src/lib.rs`, `crates/board-api/src/sources.rs`, `crates/board-api/src/openapi.rs`, `crates/board-api/src/catalog.rs`, and API tests.
4. **CLI (`crates/board-cli/src/`)**:
   - Remove `Command::Rules` and `RuleCommand` from `crates/board-cli/src/cli.rs` and their dispatch arms in `crates/board-cli/src/main.rs`.
   - Remove `Sources::Renderer` from `crates/board-cli/src/sync_cli.rs` and its handler.
   - Remove Rhai rule tests from `crates/board-cli/tests/delegation.rs`.

### 1.2 Retire 1:1 Vercel parity tracking files

Delete these obsolete parity-audit artifacts (they no longer reflect project direction):
- `PARITY.md`
- `docs/RULES.md`
- `docs/PARITY-AUDIT.md`
- `docs/CORE-COVERAGE.md`
- `docs/WORK-COVERAGE.md`
- `docs/SOURCE-COVERAGE.md`
- `docs/INSPECTOR-COVERAGE.md`
- `docs/INSPECTOR-VISUALS.md`
- `docs/REFERENCE-CHECKS.md`
- `scripts/compare-*.mjs` and `scripts/run-reference-conformance.mjs`

Keep `scripts/roundtrip.py`, `scripts/test-inspector.py`, and `scripts/inspect-source-settings.py`.

### Workstream 1 exit check
- `rg -i rhai Cargo.toml crates/ ui/ docs/` returns no code/dependency matches.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass cleanly.

---

## Workstream 2: SQLite schema v15 & domain engine (`board-core`)

Bump `DB_VERSION` in `crates/board-core/src/core.rs` from `14` to `15`. Add two new public modules in `crates/board-core/src/lib.rs`:
- `pub mod roles;` (`crates/board-core/src/roles.rs`)
- `pub mod swarm;` (`crates/board-core/src/swarm.rs`)

### 2.1 Schema v15 DDL

Run inside the existing immediate migration transaction in `Board::from_conn`:

```sql
-- Reusable role templates (built-in + custom)
CREATE TABLE IF NOT EXISTS agent_roles (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    is_builtin INTEGER NOT NULL DEFAULT 0,
    system_prompt TEXT NOT NULL,
    default_system_prompt TEXT,
    version INTEGER NOT NULL DEFAULT 1,
    updated_at TEXT NOT NULL
);

-- Agent Skills bundles (SKILL.md + optional reference/asset text files)
CREATE TABLE IF NOT EXISTS skill_bundles (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    is_builtin INTEGER NOT NULL DEFAULT 0,
    version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS skill_bundle_files (
    bundle_id INTEGER NOT NULL REFERENCES skill_bundles(id) ON DELETE CASCADE,
    rel_path TEXT NOT NULL,
    content TEXT NOT NULL,
    PRIMARY KEY (bundle_id, rel_path)
);

CREATE TABLE IF NOT EXISTS role_skills (
    role_id INTEGER NOT NULL REFERENCES agent_roles(id) ON DELETE CASCADE,
    bundle_id INTEGER NOT NULL REFERENCES skill_bundles(id) ON DELETE CASCADE,
    PRIMARY KEY (role_id, bundle_id)
);

-- Swarm runs
CREATE TABLE IF NOT EXISTS swarm_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    slug TEXT NOT NULL UNIQUE,
    goal TEXT NOT NULL,
    channel TEXT NOT NULL REFERENCES channels(name),
    repo_root TEXT,
    coordinator_actor TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('active', 'completed', 'stopped')),
    max_notifications INTEGER NOT NULL DEFAULT 200,
    notifications_sent INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    ended_at TEXT
);

-- Workers participating in a run (pinned role/skill snapshot)
CREATE TABLE IF NOT EXISTS swarm_workers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id INTEGER NOT NULL REFERENCES swarm_runs(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    actor TEXT NOT NULL,
    role_slug TEXT,
    pinned_role_version INTEGER,
    pinned_system_prompt TEXT NOT NULL DEFAULT '',
    pinned_skills_json TEXT NOT NULL DEFAULT '[]',
    brief TEXT NOT NULL,
    workspace_path TEXT,
    branch_name TEXT,
    status TEXT NOT NULL CHECK(status IN ('working', 'waiting', 'done', 'stopped')),
    waiter_pid INTEGER,
    last_read_cursor INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (run_id, name)
);
CREATE INDEX IF NOT EXISTS swarm_workers_by_run ON swarm_workers(run_id, id);

-- Direct messages within a swarm run (always visible in Rune; ordered via event_cursor)
CREATE TABLE IF NOT EXISTS swarm_dms (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id INTEGER NOT NULL REFERENCES swarm_runs(id) ON DELETE CASCADE,
    from_actor TEXT NOT NULL,
    to_worker TEXT NOT NULL,
    text TEXT NOT NULL,
    event_cursor INTEGER NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS swarm_dms_by_run ON swarm_dms(run_id, event_cursor);

-- Unified wake/notification inbox (created from @mentions on swarm posts and from swarm DMs)
CREATE TABLE IF NOT EXISTS swarm_notifications (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id INTEGER NOT NULL REFERENCES swarm_runs(id) ON DELETE CASCADE,
    target_worker TEXT NOT NULL,
    source_kind TEXT NOT NULL CHECK(source_kind IN ('mention', 'dm', 'system')),
    from_actor TEXT NOT NULL,
    message_id INTEGER REFERENCES messages(id),
    dm_id INTEGER REFERENCES swarm_dms(id),
    event_cursor INTEGER NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS swarm_notif_lookup
    ON swarm_notifications(run_id, target_worker, event_cursor);
```

**Why `event_cursor` on `swarm_dms` and `swarm_notifications`?**
Every swarm post and every swarm DM appends an entry to the existing `events` table (`message.posted`, `swarm.dm`, `swarm.status`), producing a single monotonically increasing integer `cursor`. That means `swarm read --after <cursor>` and `swarm wait --after <cursor>` use one unified cursor across public posts, DMs, and run-stop notices—never skipping concurrent messages.

### 2.2 Built-in roles and skills seeding (`crates/board-core/src/roles.rs`)

Seed the six built-in roles on v15 init (`INSERT OR IGNORE`):
1. `implementer` — scoped implementation, running tests, citing commit/diff and known limits.
2. `verifier` — independent verification against criteria, emitting `verdict@1` (`pass`/`fail`/`not-run`), exact commands run, and evidence.
3. `reviewer` — code/design review without editing the patch, citing severity and file/line locations.
4. `architect` — decomposing ambiguous work, naming tradeoffs, interfaces, and worker slices.
5. `documenter` — scoped doc updates with verified examples.
6. `designer` — UI/UX flow, layout states, acceptance criteria, and visual captures.

Also seed concise default instruction-only `SKILL.md` bundles attached to their matching roles, plus a constant `SWARM_WORKER_PROTOCOL` prepended/appended to every worker's rendered prompt teaching:
- stay within brief ownership/exclusions,
- work autonomously through full edit/test loops before posting,
- use `@name` in `board swarm post` **only** to notify/wake that teammate (use plain name without `@` for FYI references),
- use `board swarm dm --to <name>` for side conversations,
- wait via `board swarm wait --after <cursor>` and exit when `status == "stopped"`.

**Validation & safety rules for skill bundles:**
- Slug regex: `^[a-z0-9][a-z0-9_-]{0,63}$`.
- Bundle must contain `SKILL.md` at root.
- Relative paths only: reject leading `/`, `..`, backslashes, NULs, hidden dotfiles, or symlinks outside the bundle directory.
- Size limits: max 32 files per bundle, max 256 KiB per file, max 1 MiB total per bundle, UTF-8 text only.
- `delete_skill_bundle(slug, force: bool)`: returns the list of roles referencing the bundle; if `force == false` and roles still reference it, returns a `Conflict` error listing affected role slugs so the CLI/UI can prompt clearly.

### 2.3 Core swarm operations (`crates/board-core/src/swarm.rs`)

Implement transactional methods on `Board`:
- `swarm_start(actor, SwarmStart { goal, slug, repo_root, max_notifications }) -> SwarmRun`:
  - Creates channel `swarms/<slug>` (auto-generating a unique slug if omitted, e.g. `run-<short-id>`).
  - Records `coordinator_actor = actor`, `status = 'active'`.
  - Posts an initial root goal message to `swarms/<slug>`.
- `swarm_add_worker(actor, run_ref, SwarmAddWorker { name, role_slug, brief, workspace_path, branch_name }) -> SpawnedWorker`:
  - **Authority check:** `actor` must match `run.coordinator_actor` or be the owning person (a sibling worker profile cannot add another worker).
  - Validates worker name (`^[a-z0-9][a-z0-9_-]{0,23}$`, disallowing `all`).
  - Resolves `role_slug` (if provided) and pins `pinned_role_version`, `pinned_system_prompt`, and `pinned_skills_json` (JSON array of `{ slug, version, files }`).
  - Materializes pinned skill bundles into `<board-data-dir>/swarms/<run-slug>/<worker-name>/skills/<skill-slug>/...` (mode `0700` on Unix).
  - Posts the worker's public brief to `swarms/<slug>` as `actor` mentioning `@<worker-actor>` and returns `SpawnedWorker { worker, assembled_prompt, skills_dir, env_vars }`.
- `swarm_post(actor, run_ref, text, PostOptions) -> SwarmPostResult`:
  - Posts to `run.channel` via existing transactional message write (resolving `@mentions` against run workers + visible directory).
  - For each resolved mention matching an active worker in `run_id` (excluding self), inserts a `swarm_notifications` row (`source_kind = 'mention'`) in the same transaction and increments `notifications_sent`.
  - If `notifications_sent > max_notifications`, still posts the message (or alerts `coordinator` via a `system` notification) and refuses further peer notifications until coordinator intervenes.
- `swarm_dm(actor, run_ref, to_worker, text) -> SwarmDm`:
  - Validates `to_worker` is either `"coordinator"` or a registered worker in `run_id`.
  - Appends a `swarm.dm` event to `events` to obtain `event_cursor`, inserts into `swarm_dms`, and inserts a `swarm_notifications` row (`source_kind = 'dm'`) in the same transaction.
- `swarm_read(actor, run_ref, after_cursor, limit, include_dms_for_all: bool) -> SwarmFeed`:
  - Returns interleaved public messages in `run.channel` and DMs after `after_cursor`, ordered by `event_cursor`.
  - Visibility rule for DMs: participants (`from_actor` / `to_worker`), the `coordinator_actor`, and the board owner (including Rune UI) see DMs; unrelated workers only see DMs they are party to unless `include_dms_for_all` is set by owner/coordinator.
- `swarm_poll_wait(actor, run_ref, target_worker, after_cursor) -> WaitPoll`:
  - Checks if `run.status != 'active'` → returns `WaitPoll::Stopped { cursor }`.
  - Queries `swarm_notifications` for `(run_id, target_worker)` with `event_cursor > after_cursor` → if any exist, fetches the matching messages/DMs and returns `WaitPoll::Ready { cursor, notifications, messages, dms }`.
  - Otherwise returns `WaitPoll::Pending`.
- `swarm_set_worker_status(actor, run_ref, worker_name, status, waiter_pid)`:
  - Updates `swarm_workers.status`, `waiter_pid`, and `updated_at` (called on `wait` entry/exit and `done`).
- `swarm_stop(actor, run_ref)`:
  - **Authority check:** coordinator or board owner only.
  - Marks `swarm_runs.status = 'stopped'`, sets `ended_at`, marks non-done workers `stopped`, appends a `swarm.stopped` event so any blocked `swarm wait` wakes and exits immediately.

### 2.4 Built-in coding-swarm schemas (`crates/board-core/src/schemas.rs`)

Add two built-in schemas alongside `git-commit@1` and `deployment@1`:
- **`verdict@1`**:
  ```json
  {
    "type": "object",
    "required": ["status", "summary"],
    "properties": {
      "status": { "type": "string", "enum": ["pass", "fail", "not-run"] },
      "artifact": { "type": "string" },
      "summary": { "type": "string" },
      "checks": {
        "type": "array",
        "items": {
          "type": "object",
          "required": ["name", "status"],
          "properties": {
            "name": { "type": "string" },
            "status": { "type": "string", "enum": ["pass", "fail", "not-run"] },
            "command": { "type": "string" },
            "output": { "type": "string" }
          }
        }
      }
    }
  }
  ```
- **`diff@1`**:
  ```json
  {
    "type": "object",
    "required": ["patch"],
    "properties": {
      "base": { "type": "string" },
      "head": { "type": "string" },
      "files": { "type": "array", "items": { "type": "string" } },
      "patch": { "type": "string" }
    }
  }
  ```

---

## Workstream 3: CLI surface, bundled harness skills & `--as-tasks` sync (`board-cli`)

### 3.1 New CLI modules

Add `crates/board-cli/src/swarm_cli.rs`, `crates/board-cli/src/roles_cli.rs`, and `crates/board-cli/src/skill_install.rs`, wired into `Command` in `crates/board-cli/src/cli.rs`:

- `board swarm <subcommand>`:
  - `start <goal> [--slug SLUG] [--repo PATH] [--max-notifications N]`
  - `add <name> [--run RUN] [--role SLUG] --brief TEXT [--workspace PATH] [--branch NAME]`
  - `status [RUN]`
  - `post [TEXT] [--run RUN] [--file PATH] [--diff [RANGE]] [--attach FILE]... [--schema NAME@VER]` *(strictly NO `--to` flag)*
    - When `--diff` is passed, runs `git diff [RANGE]` in the current directory, caps output cleanly (or attaches larger diffs as a `.diff` attachment), and appends a `json#diff@1` block.
  - `dm --to <worker|coordinator> [TEXT] [--run RUN] [--file PATH]` *(`--to` required)*
  - `read [--run RUN] [--after CURSOR] [--limit N]`
  - `wait [--run RUN] [--after CURSOR] [--timeout 5m]`
    - Resolves `target_worker` from `BOARD_PROFILE` (or `"coordinator"` if caller matches `run.coordinator_actor`).
    - Sets worker status to `waiting` with `std::process::id()` using a RAII guard that restores `working` (or clears `waiter_pid`) on exit or SIGINT/SIGTERM.
    - Polls `swarm_poll_wait` every 250ms until notifications arrive, run stops, or timeout expires.
    - Exits `0` in all three cases (`messages`, `still_waiting`, `stopped`) with clean JSON when `--json` is set.
  - `done [SUMMARY] [--run RUN]` — posts optional summary and marks worker `done`.
  - `stop [RUN]` — coordinator/owner only.
- `board roles <list|show|create|edit|reset|delete|attach-skill|detach-skill>`
- `board skills <list|show|import|delete>`
- `board skill install <claude|codex|path> [--dest PATH]`
  - Embeds the coordinator `SKILL.md` via `include_str!("../assets/board-swarm-SKILL.md")`.
  - Default destinations:
    - `claude` → `~/.claude/skills/board-swarm/SKILL.md`
    - `codex` → `~/.codex/skills/board-swarm/SKILL.md`
    - `path` → `<dest>/board-swarm/SKILL.md`
  - Teaches the host harness how to: start a run, choose roles, call `board swarm add` (or MCP `swarm_add`), spawn native background subagents with the returned prompt + `BOARD_SWARM_RUN` + `BOARD_PROFILE`, wait for completion/questions, and call `board swarm stop`.

### 3.2 Ticket-backlog sync (`--as-tasks`)

In `crates/board-core/src/sources/queue.rs` and `crates/board-cli/src/sync_cli.rs`:
- Add `--as-tasks` flag to `board sync pull <source> [thread]`.
- When enabled, any **newly created root message** imported during the drain is promoted to a Board task (`tasks::create_from_root`) inside the same SQLite write transaction, inheriting the issue's imported labels (e.g. `tag.*`, `state=*`).
- Idempotent on replay: already-imported roots are not duplicated or reset if already claimed/completed.

---

## Workstream 4: Stdio MCP server (`crates/board-mcp`)

Create a new workspace crate `crates/board-mcp` (add to `Cargo.toml` workspace members) and wire `board mcp` in `board-cli`.

### 4.1 Architecture

Separate protocol framing from tool execution so future hosted HTTP/SSE MCP can reuse the exact same handler:
- `crates/board-mcp/src/router.rs` — Transport-agnostic JSON-RPC 2.0 dispatcher (`handle_request(&Board, &ActorContext, Request) -> Option<Response>`).
- `crates/board-mcp/src/tools.rs` — Tool definitions (names, descriptions, JSON Schemas) and execution handlers calling `board-local`.
- `crates/board-mcp/src/stdio.rs` — Newline-delimited JSON reader/writer over `stdin`/`stdout` (all diagnostic logs strictly to `stderr`).

### 4.2 Supported JSON-RPC methods

- `initialize` — returns server info (`board-mcp`, workspace version) and `capabilities: { tools: {} }`.
- `notifications/initialized` — no-op notification.
- `ping` — returns `{}`.
- `tools/list` — returns schemas for all tools below.
- `tools/call` — dispatches tool, returns `{ content: [{ type: "text", text: "<json>" }], isError: bool }`.

### 4.3 MCP tool inventory

| Tool | Required args | Optional args |
| --- | --- | --- |
| `swarm_start` | `goal` | `slug`, `repo_root`, `max_notifications` |
| `swarm_add` | `name`, `brief` | `run`, `role`, `workspace_path`, `branch_name` |
| `swarm_status` | — | `run` |
| `swarm_post` | `text` | `run`, `labels`, `schema`, `schema_data` |
| `swarm_dm` | `to`, `text` | `run` |
| `swarm_read` | — | `run`, `after`, `limit` |
| `swarm_wait` | — | `run`, `after`, `timeout_seconds` (default `90`, max `300`) |
| `swarm_done` | — | `run`, `summary` |
| `swarm_stop` | — | `run` |
| `roles_list` | — | — |
| `roles_show` | `slug` | — |
| `board_channels` | — | `prefix`, `limit` |
| `board_post` | `channel`, `text` | `labels`, `no_sync` |
| `board_reply` | `id`, `text` | `labels`, `no_sync` |
| `board_read` | — | `target`, `channel`, `text`, `by`, `mentions`, `after`, `limit` |
| `tasks_list` | — | `channel`, `state`, `assignee`, `labels`, `limit` |
| `tasks_create` | `channel`, `text` | `assignee`, `needs`, `labels`, `claim` |
| `tasks_claim` | — | `id`, `ready_channel`, `labels`, `lease_seconds` |
| `tasks_complete` | `id` | `result` |
| `tasks_fail` | `id`, `reason` | — |
| `tasks_block` | `id`, `reason` | — |

Environment resolution on `board mcp` startup:
- Honors `--db` / `BOARD_DB`, `--name` / `BOARD_NAME`, `BOARD_USER`, `--as` / `BOARD_PROFILE`, `BOARD_SWARM_RUN`, and `BOARD_TOKEN` (when running as a delegated credential).
- Tool calls may pass an explicit `run` argument or fall back to `BOARD_SWARM_RUN`.

---

## Workstream 5: Rune desktop UI (`ui/`)

Add swarm visibility and role/skill configuration to `board-ui` while preserving the strict invariant: **opening or refreshing Rune never starts, resumes, or wakes any agent.**

### 5.1 Navigation and state (`ui/src/state.rs`, `ui/src/view/shell.rs`)

- Extend `Page` enum with:
  - `Page::Swarms` (list of runs + active run detail)
  - `Page::Swarm(i64)` (selected run)
  - `Page::Roles` (under **Settings > Agent Roles**)
- Add **Swarms** as a top-level item in the sidebar (`ui/src/view/shell.rs`), showing an active-run count badge.
- Add **Agent Roles** tab/section in `ui/src/view/workspace/admin.rs` (Settings).

### 5.2 Swarm view (`ui/src/state/swarms.rs`, `ui/src/view/swarms.rs`)

- **Polling:** Hook into the existing ~1s refresh tick when `Page::Swarms | Page::Swarm(_)` is active. Re-query runs, workers, and feed entries directly from SQLite.
- **Stale waiter check:** On Unix, if a worker row has `status == 'waiting'` and a `waiter_pid`, check `kill(pid, 0)` (read-only liveness check, or display `"offline"` if PID no longer exists and `updated_at` is stale).
- **Layout (wide desktop split, compact single-pane):**
  - Left/top: Run selector showing slug, goal, status badge (`active` / `completed` / `stopped`), and activity count.
  - Roster panel: Each worker card shows `@name`, role pill, live state indicator (`working`, `waiting`, `done`, `stopped`), branch/workspace if set, and a toggle to expand the worker's full brief. Clicking a worker filters the feed to messages involving that worker.
  - Unified feed: Chronological merge of public channel messages and `swarm_dms` sorted by `event_cursor`.
    - Public posts with `@mentions` display a subtle `notified @verifier` badge.
    - DMs display a distinct `DM @impl → @verifier` header pill.
    - Filter pills: `All` | `Public` | `DMs` | active worker filter.
  - Composer & controls at bottom:
    - Target selector: `Post to board` (public) or `DM -> @<worker>` (dropdown of workers + coordinator).
    - Confirmed **Stop run** button in the run header when `status == 'active'`.

### 5.3 Rich diff & verdict cards (`ui/src/view/rich.rs`)

Extend the existing validated block renderer:
- **`verdict@1`**: Render a compact status header (`PASS` / `FAIL` / `NOT RUN`), optional artifact/commit chip, summary text, and expandable check rows (`name`, `status`, `command`, `output`), keeping the existing raw-JSON toggle.
- **`diff@1`**: Render `base..head` + file list + monospace unified diff lines with subtle light/dark green/red tinting for `+` / `-` lines (respecting `dark()` theme tokens in `ui/src/view.rs`).

### 5.4 Settings > Agent Roles (`ui/src/state/roles.rs`, `ui/src/view/roles.rs`)

- **Role sidebar/list:** Shows all built-in roles (with `Built-in` pill) and custom roles, plus **+ New role**.
- **Role editor:**
  - Name, slug (read-only for built-ins), description, multiline system prompt `TextEdit`.
  - **Save** button (bumps `version`), **Restore default** button (for built-ins, with confirmation), **Delete role** (for custom roles only).
- **Skills section (per role + library):**
  - Lists attached skills with a **Detach** button (explicitly labeled *"Remove from this role"*).
  - **Attach existing skill** picker for skills already in the SQLite library.
  - **Import skill folder** button (uses Rune's native directory/file dialog or path input, validates `SKILL.md` + files, stores in SQLite, attaches to current role).
  - **Library view** with **Delete from library** action: if any roles still reference the skill, shows a confirmation dialog listing every affected role before deleting.

---

## Workstream 6: Verification matrix

All checks run offline with `cargo test --workspace` (no live external model API calls required for automated CI):

1. **Core tests (`crates/board-core/tests/swarm_and_roles.rs`)**:
   - Schema upgrade from a populated v14 database to v15 (verifies legacy Rhai tables dropped cleanly, 6 built-in roles seeded, existing messages/tasks intact).
   - Role edit, version bump, built-in reset, custom role create/delete.
   - Skill bundle import validation (rejects `..`, missing `SKILL.md`, oversized files), multi-role attach/detach, guarded delete listing referencing roles.
   - Snapshot pinning: editing a role or deleting a skill does not alter an already-spawned worker's pinned prompt/skills.
   - Notification semantics: public post without `@mention` creates **0** notifications; public post with `@verifier` creates **1** notification; `swarm_dm` creates **1** notification and increments monotonic `event_cursor`.
   - Authority: worker profile cannot call `swarm_add_worker` or `swarm_stop` (`Forbidden`).
   - `sync pull --as-tasks` creates tasks once and is idempotent on replay.
2. **CLI & cross-process tests (`crates/board-cli/tests/swarm_cli.rs`)**:
   - Spawn `board swarm wait` in a background child process; verify worker status flips to `waiting` in SQLite; post `@worker` from a second process; verify `wait` unblocks immediately, flips status back to `working`, and returns the message.
   - Verify `board swarm post` rejects `--to` and `board swarm dm` requires `--to`.
   - Verify `board swarm stop` immediately unblocks any waiting worker with `status == "stopped"`.
   - Verify `board skill install path --dest <tmp>` writes valid `board-swarm/SKILL.md`.
3. **MCP tests (`crates/board-mcp/tests/stdio.rs`)**:
   - Drive `board mcp` over stdin/stdout pipes: `initialize` → `tools/list` → `swarm_start` → `swarm_add` → `swarm_post` → `swarm_dm` → `swarm_read` → `swarm_wait` (timeout + live wakeup) → `tasks_create` / `tasks_claim` / `tasks_complete` → `swarm_stop`.
4. **Rune UI tests (`ui/tests/inspector/swarms_and_roles.rs`)**:
   - Render Swarm View with active run, two workers (`working` and `waiting`), public posts, `@mention` notification, `DM`, `verdict@1`, and `diff@1` blocks; verify element tree and PNG render.
   - Send a human post and a human DM from Rune; verify SQLite writes and zero auto-wakes on open.
   - Open **Settings > Agent Roles**, edit a built-in prompt, restore default, import a skill bundle into two roles, detach from one, delete from library (asserting multi-role warning appears), restart `board-ui`, and verify persisted state + preserved worker snapshot.
5. **Manual end-to-end acceptance test**:
   - Run `board skill install claude` (or `codex`), start a real coding session on a sample task with one `implementer` and one `verifier` subagent, watch the `@impl ↔ @verifier` loop live in `board-ui`, verify zero coordinator round-trips during peer iteration, and verify clean stop when finished.
