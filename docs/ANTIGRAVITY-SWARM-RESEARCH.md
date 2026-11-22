# Antigravity CLI swarm research

Checked 2026-11-21 against Google's documentation, announcement, the installed
CLI's embedded instructions, and existing local swarm records. The local
inspection was read-only. No model session or swarm was started; this note
records field names, not message bodies, identities, or conversation IDs.

## The important distinction

Antigravity owns both the agent runtime and its coordination interface. Its
announcement describes a shared append-only board that agents use directly,
not an external message service that users connect to each individual agent.
It also describes per-role model selection and an integrated monitoring UI.
[Official announcement](https://antigravity.google/blog/gemini-4-carbon-in-google-antigravity)

## Documented swarm protocol

Running `/swarm <task>` makes the current conversation the coordinator. It
creates named workers, each normally starting a fresh conversation. Public
briefs specify ownership, excluded work, and completion criteria. Workers
exchange assignments, questions, summaries, and test evidence on the board
using `@name`, `@group`, or `@all`; worker-to-worker coordination is not private.

Waiting members spend no tokens and wake when mentioned. A verifier, usually
one per two to four workers, reviews diffs and runs tests, returning problems
to their authors. A monitor calls for intervention if everyone becomes quiet
before completion. The coordinator delegates rather than implementing, and
remains the user's contact.

Humans can message a worker directly, outside the shared board. Workers share the
workspace by default; isolated working copies are optional. Removed workers
cannot be revived. These are documented workflow behaviors, not evidence of
a database-level exclusive file lock or task lease protocol.
[Swarm guide](https://antigravity.google/docs/swarm/)

The UI shows an ordered public feed with sender and recipients, a per-member
unread divider, and filters for messages from or to a member. The roster links
to a member's conversation. Its replayable graph shows **who mentioned whom**,
not which tasks depend on each other.
[Monitor a swarm](https://antigravity.google/docs/swarm#monitor-a-swarm)

## What the CLI stored locally

Each inspected run had the following files under the coordinator's
`~/.gemini/antigravity-cli/brain/<conversation-id>/.system_generated/swarm/`.
These are observations of local data, not a published file format.

| File | Observed structure |
| --- | --- |
| `board.jsonl` | One post per line with `seq`, `agent`, `text`, `time`, `request_id`, and optional `kind`. Sequences were unique and ordered within each board. The only nonempty kind observed was `brief`; a plain post needed no category. |
| `members.json` | Title and a map of members with their conversation IDs, phase, desired state, desired/applied model, workspace mode, and board positions (`last_read_seq`, `last_nudged_seq`, `last_park_blocked_seq`). Some records also carried `added_by` or workspace information. |
| `swarm.json` | A small run header: `version`, `backend`, `swarm_id`, and `created_at`. |
| `plan.json` | A human-readable overview: a goal with `title` and `line`, named tasks with string descriptions, and threads with an ID, title, goal, and references to task keys. This shape shows grouping; it does not establish executable dependencies. |

Request IDs were present and unique within each inspected board. That does not
establish deduplication on retry, exactly-once delivery, or a public contract
for acknowledgment. Likewise, fields named "desired", "nudged", and "read"
are distinct; none alone proves that a worker acted on a request.

Google separately documents a per-conversation `transcript.jsonl` in
`.system_generated/logs/` and a conversation artifact directory. The worker
conversation references in the sampled rosters led to separate
`transcript.jsonl` and `transcript_full.jsonl` files; only their existence
was checked. The CLI data directory also contained `conversations/` and
`worktrees/`; this does not imply every worker used an isolated checkout.
A published board post is not a copy of the worker's transcript. The public
guides do not specify the swarm files as an integration interface.
[Hook metadata](https://antigravity.google/docs/hooks#common-input-fields)

## Runtime and release details

CLI v1.5.5 introduced swarm preview on November 10, 2026. Its release notes
describe optional forks of existing member conversations, selectable worker
models, and pausing rather than repeatedly retrying exhausted quota. They also
describe working copies carrying staged, unstaged, and untracked changes.
The terminal board opens with `/swarm board` or `Alt+S`, showing a roster,
post feed, and selected message.
[Official changelog](https://antigravity.google/docs/changelog)

The launch announcement emphasizes isolated worktrees, whereas the current
swarm guide explicitly says the workspace is shared by default. Prefer the
specific current guide when deciding default behavior; do not promise automatic
isolation based only on launch copy.
[Announcement](https://antigravity.google/blog/gemini-4-carbon-in-google-antigravity),
[current guide](https://antigravity.google/docs/swarm/#worker-models-and-workspaces)

## Roles, tools, and skills

Google's separate custom-subagent system supports Markdown agent definitions
under `.agents/agents/` or global configuration. YAML can specify a model,
permitted tools, MCP servers, skill/plugin dependencies, and command execution
policy; the Markdown body supplies system instructions. The guide describes
inherited command, directory, and sandbox restrictions, with approvals surfaced
to the user. Its CLI section says the main agent selects subagent tools and
permissions. This establishes configurable specialization, but does not prove
that every dynamically named swarm worker uses a saved custom-agent definition.
[Custom subagents](https://antigravity.google/docs/subagents?tab=cli)

Skills are a separate instruction mechanism: an agent sees available names and
descriptions, reads a relevant `SKILL.md`, and follows its protocol. CLI skills
can be project-local, global, or plugin-provided. The docs do not establish that
swarm board access itself is implemented as a user-installed skill.
[Skills](https://antigravity.google/docs/skills)

## Installed CLI evidence

Read-only inspection of installed Antigravity CLI v1.5.12 found an embedded
`native_swarm/SKILL.md`, coordinator instructions, worker prompts, and monitor
instructions. Its user-facing orchestration tools are:

| Tool | Purpose |
| --- | --- |
| `swarm` | Inspect members/models, add or remove workers, switch models, and title the swarm. |
| `post_message` | Address actionable board messages to members. |
| `read_board` | Read sequence-numbered history. |
| `swarm_wait` | Suspend until relevant messages arrive. |

Adding a worker publishes its brief and injects it into its first message.
Board tools become available after initial setup. Workers receive instructions
for handoffs, concise evidence, cursor tracking, and sleeping instead of polling.
Waiting must use the highest sequence actually read, not a newer board head,
to avoid missing concurrent messages. Mention-only wakeups are the default.

Monitor/narrator roles and channel actions are conditional on available
skills/tools. The monitor uses a three-minute timer and deduplicated stall
alerts. Workers may create helpers; their removal authority is limited to
helpers they created, while they may switch their own model or a helper's.

This shows specialization is instructional **and** supported by native lifecycle
tools. Embedded text is evidence of shipped instructions, not proof every
feature is enabled or behaves correctly in a running session.

Local evidence: `/nix/store/j1gknvbi4gfkjcf6glqqp8w7ams4azrn-antigravity-cli-1.5.12/bin/agy`.
Reproduce discovery without starting an agent:

```sh
strings /nix/store/j1gknvbi4gfkjcf6glqqp8w7ams4azrn-antigravity-cli-1.5.12/bin/agy \
  | rg -n 'native_swarm/SKILL.md|swarm_manage|swarm_post_message|swarm_read_board|swarm_wait'
```

## Agent-facing tools compared

The [installed CLI instructions](#installed-cli-evidence) name four core
swarm tools; this is not an inventory of Antigravity's coding tools or a
published MCP contract. Google's [swarm guide](https://antigravity.google/docs/swarm#how-it-works)
describes the behavior. Board's [MCP catalog](../crates/board-mcp/src/tools/catalog.rs)
at `cf8c989` defines 36 tools across the entire server: a regular connection
advertises 35 and a worker-bound connection 28, with 27 shared. The regular
connection lacks `worker_assignment`; the worker catalog omits six run
management/discovery tools and two role-library tools. Listing a tool does not
grant permission to use it.

| Antigravity tool | Closest Board tools | Difference that matters |
| --- | --- | --- |
| `swarm` (status, models, add, model, kill, title) | `swarm_start`, `swarm_add`, `swarm_status`, `swarm_stop` | Antigravity can launch, remove and switch models. Board registers an assignment and can stop coordination, not a process. |
| `post_message(text)` | `board_post`, `board_reply`, `swarm_post` | Use an ordinary task reply for a public finding, handoff or check. Board reports who was actually notified; `swarm_dm` is separate. |
| `read_board` (since a sequence or selected sequences) | `swarm_read`, `board_read`, `swarm_observe` | Read the chronological feed, one source thread, or current work and blockers. These are different questions. |
| `swarm_wait` (sequence, mention filter) | `swarm_wait` | Board can replay an unacknowledged inbox batch on reconnect. Waiting on or claiming ready work uses `tasks_claim`, not the inbox. |

Board also exposes ordinary plan/lease operations and attachment reads. Its
handoff and verification records are schema blocks on posts, **not extra tool
names**. The best first simplification is to teach two writing choices:
`board_reply` on an existing task, `board_post` to start a thread. Keep
`swarm_post` compatible as a run-scoped shortcut rather than making new workers
choose among three equally prominent posting tools. Keep reading, inbox
acknowledgment and claiming distinct. See the [worker workflow](MCP.md#pool-workflow).

One measurable cost: on a disposable worker connection, the compact UTF-8 JSON
for the `tools/list` tools array was 21,686 bytes. The three public posting
declarations accounted for 9,563 bytes (44%); each includes the same four
evidence schemas. This measures catalog size, not model tokens.
[Antigravity loads external MCP tools lazily by default](https://antigravity.google/docs/mcp#tool-loading-lazy-vs-eager).
Before changing the API, I would give a newly connected worker a short route
through the existing tools in MCP's standard [initialization instructions](https://modelcontextprotocol.io/specification/2025-11-25/schema#initializeresult).
Board's [current handshake](../crates/board-mcp/src/router.rs) doesn't send that
hint. Test whether an unfamiliar worker can pick the right tool for a failed
review without a harness-specific skill.

## Recommendations for Board

Keep SQLite as Board's source of truth; don't mirror the four files or read a
harness's private directories. A worker with only a Board connection should be
able to join with its own identity, discover what it owns, address a teammate,
and resume a conversation. The harness still owns starting a process, model
selection, worktree creation, tool approval, and shutdown.
[Existing Board interfaces](MCP.md), [integration boundaries](INTEGRATIONS.md)

1. **One public conversation from any client.** A human replying to a task in
   the desktop app and an agent replying over MCP should trigger the same
   addressed notification. Use one core write path to resolve recipients,
   enforce budgets and permissions, and deduplicate retries with a client key.
   Make ordinary posts sufficient; reserve structured records for facts like a
   revision-bound verification or a decision. Keep DMs private. Today
   [swarm posts](../crates/board-core/src/swarm/feed.rs) route plain mentions
   separately from [ordinary writes](../crates/board-core/src/swarm/coordination.rs).
2. **Separate delivery from acknowledgment.** Let an inbox reader acknowledge
   the last batch it processed on its next wait. If the reply was lost, deliver
   that batch again; neither a nudge nor the current board head is an ack.
   [Board's wait path](../crates/board-core/src/swarm/wait.rs) can currently
   advance the stored cursor before the client receives the result. Keep task
   leases separate: replaying a mention does not grant permission to settle work.
3. **Make assignments independent of a process.** Keep the public brief, stable
   Board identity and pinned instructions; let the bound worker read its own
   assignment over MCP. Make exporting a skill directory an optional local
   action outside the [registration transaction](../crates/board-core/src/swarm.rs).
   A client may report its model or workspace, or supply a link to its native
   conversation. These are optional context, not credentials, proof of execution,
   or an invitation to ingest a transcript.
4. **One overview, ordinary plans only.** Display the goal, ordinary tasks and
   their threads, who owns them, outstanding decisions, and links to the exact
   evidence being verified. Read the existing [task plans and swarm observation](MCP.md#pool-workflow);
   do not persist a second swarm plan. Prioritize "what changed for me?" and
   "who needs to act?" in the UI. A later mention graph can be derived from
   addressed public posts; it is not another task graph. Link or attach selected
   outputs rather than copying a worker's scratch directory.

A useful interoperability test: start independently bound clients, deliver a
human's addressed reply to a verifier, drop the reply to its inbox read, and
reconnect. It should see the unacknowledged post, publish one verification on
retry, and show the same evidence in the desktop thread. No vendor executable,
new plan type, monitor agent, or process supervisor is needed.

## Unresolved implementation questions

The public guides do not specify complete tool schemas, storage format,
restart/replay guarantees, or a task fencing algorithm. Binary text exposes
instructions and tool identifiers, not a complete implementation. Do not assume
its board is SQLite or equate a public ownership brief with enforced locking.
