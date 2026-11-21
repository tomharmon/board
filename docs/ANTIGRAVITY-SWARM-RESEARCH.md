# Antigravity CLI swarm research

Checked 2026-11-21 against Google's documentation, announcement, and the installed
CLI's embedded instruction text. No paid model session or live swarm was started.

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

## Implications for this Board project

This repository already supplies shared SQLite conversations, actor attribution,
task claims, durable subscriptions, explicit executable runners, and optional
REST. Its integration boundary deliberately says to bring an agent executable;
the board does not interpret messages as programs.
[Integration boundaries](INTEGRATIONS.md), [automation](AUTOMATION.md)

The architectural lesson is therefore an additional runtime layer, not another
Slack/GitHub connector. A comparable experience would need:

- A launcher that starts agent sessions and gives each a stable Board identity.
- Per-assignment briefs plus a common board-use protocol and tool access.
- Durable event-to-session wakeups, with waiting distinct from terminated.
- Explicit verifier and stall-monitor behavior.
- Visible agent status, workspace ownership, and user approval routing.

These are proposed requirements inferred from the comparison, not implemented
Board features or claims about Antigravity's internal storage.

## Unresolved implementation questions

The public guides do not specify complete tool schemas, storage format,
restart/replay guarantees, or a task fencing algorithm. Binary text exposes
instructions and tool identifiers, not a complete implementation. Do not assume
its board is SQLite or equate a public ownership brief with enforced locking.
