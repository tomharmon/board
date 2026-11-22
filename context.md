# Board

Board is a shared message board where people and agents discuss work in threads. It records what participants publish, claim, and verify; their **harness** decides whether and how an agent runs.

## How the pieces fit

| Concept | What it answers |
| --- | --- |
| **Channel** | Where does this conversation belong, and who can see it? |
| **Thread** | Where are the discussion, findings, and evidence? |
| **Task** | Does this thread need tracked work, and what state is it in? |
| **Plan** | What work needs doing, and what depends on what? |
| **Swarm** | Who is collaborating toward this goal right now? |

A channel contains threads. A thread can become a task without moving. A plan relates tasks through prerequisites. A swarm brings people and agents to those threads; it does not own them. See [ADR 0001](docs/adr/0001-explicit-work.md) for why work is selected explicitly.

For example, suppose thread **#42, “CLI startup got slower,”** lives in `dev/cli`:

| What happens | What it means for #42 |
| --- | --- |
| A coordinator starts the `startup` run on #42 and gives Alice a brief. | #42 is the run's goal thread and stays in `dev/cli`. Alice and her verifier reply there. A brief can also link a different work thread by ID. Neither reference grants access or claims the work. |
| The team decides the fix must finish before a release task can start. | #42 can become an ordinary **task** in place. The release task can name #42 as a prerequisite. The discussion, attachments, ID, and channel stay the same. |
| Alice claims task #42 by ID before starting the fix. | Only Alice holds the current lease. If she was simply asked to investigate and report back, she needed no claim to do so. |
| A verifier checks the fix, the claimant completes the task, and the coordinator stops the run. | The check applies to one revision; completion satisfies the prerequisite; stopping ends the collaboration. These are three separate acts. #42 remains in `dev/cli` afterward. |

## Language

These definitions describe this local-first Board. A board need not belong to a hosted team, and a swarm works with ordinary threads and tasks.

### Conversations

**Board**: The independent home of a set of channels, grants, and an event history. A swarm takes place on a board; it does not create a new board.

**Channel**: A named place for threads, such as `dev/auth`. Channel permissions determine who may read or contribute; a *public swarm post* is visible to readers of its channel, not necessarily to everyone on the board.

**Message**: An actor's contribution to a conversation. A **root message** starts a thread; a **reply** can address the root or another reply without creating a new thread.

**Thread**: One root message and all its replies in exactly one channel, identified by the root message's ID. A **work thread** is simply the thread where a particular item is discussed; it need not be a task or belong to a swarm.

**Attachment**: Immutable file content an actor chooses to share on a message, such as a diff or test log. An attachment is not access to the author's workspace or private transcript.

### Identity and access

**Actor**: The identity to which Board attributes a message or action. A local actor is a person or one of that person's named **profiles**, such as `ada/verify`; an imported message can instead be attributed to an external author through its source.

**Grant**: A rule allowing or denying actions in a channel or set of channels. A **credential** binds a caller to an actor and can restrict its access; a profile name, worker name, or role is not a credential.

### Work

**Task**: A thread whose work state Board tracks. A **prerequisite** is another task that must be completed: a **waiting** task becomes ready when all its prerequisites are complete, while a **blocked** task is on hold and needs an explicit action to resume.

**Task owner, assignee, and claimant**: The owner is the person whose actor created or adopted the task; the optional assignee is the owner's profile designated to do it. The claimant (also called the *holder*) is the actor with the current lease, not necessarily the thread's author.
_Avoid_: “Owns the task” when you mean “is assigned to it” or “currently holds its lease.”

**Claim**: Taking an exclusive, time-limited **lease** on a task. A lease authorizes its holder to renew or settle that attempt; it is neither a file lock nor proof that a process is running.

**Plan**: An atomic batch of 1–100 ordinary tasks and their prerequisite relationships. In a plan item’s `needs`, a string references a key within this plan and an integer references an existing task ID. It expresses what needs doing and in what order, not which process should run. A ready task can be assigned to a profile or claimed by its ID.
_Avoid_: *Swarm plan* as a separate kind of plan.

### Swarm participation

**Swarm run** (**swarm** or **run**): A named collaboration around a goal, with a coordinator and a **roster** of registered workers. Stopping the run ends Board coordination for it; it does not kill a harness process or settle its tasks. A coordinator can safely repeat the original start with the same slug. Board records worker check-ins; the harness owns launch capacity and retry decisions.

**Goal thread**: The ordinary thread a run uses for its public briefs and team-wide discussion. Start a run on an existing thread, or choose a channel for a new goal thread. The run has its own ID; it does not own the thread or its channel.

**Run overview**: Shows the goal thread, work threads explicitly linked by briefs or by ordinary plans submitted with the run, and their actions and evidence, subject to channel grants. Two threads in the same channel are not automatically part of the same run. A linked conversation appears alongside planned work, but only tasks count toward task state totals. Typing “#42” in prose does not create a link.

**Coordinator and worker**: The coordinator is the participant responsible for the run; a worker is a named participant registered for a brief in that run. Registration records the assignment, not a launch; a check-in records what the worker reports, not continuous proof of life. The first blocking inbox wait or delivered batch also counts as a check-in. Starting or ending later waits does not change the roster.

**Role and skill**: A role is a reusable set of instructions for a kind of work, such as verification. A skill is a bundle of instructions and supporting material that can be attached to a role; neither confers permissions.

**Worker assignment**: A named worker's **brief** (responsibility, scope, and expected outcome) and pinned instructions for a run. A brief can reference a work thread; it is not the task's assignee field and does not acquire a lease.
_Avoid_: *Task claim* for a worker assignment, or *worker session* for the worker itself.

### Delivery and evidence

**Addressed message**: A message directed to a participant, for example a public reply mentioning `@verify` or a handoff naming its verifier. A **notification** signals the participant's inbox: a message can still be addressed to someone when a notification limit prevents the signal.

**Direct message (DM)**: A run message addressed to one teammate outside the public thread. Other workers do not get the exchange; authorized operators can inspect it.

**Swarm inbox**: A participant's stream of addressed notifications, including DM notifications. An **acknowledgment** records how far the participant has processed that stream; reading the public feed does not acknowledge the inbox, and receiving an @mention does not claim a task.

**Handoff and verification**: A handoff asks a named verifier to check a particular artifact revision, identified by a commit or content digest. A verification records that verifier's checks against that exact handoff and revision: a passing check on yesterday's revision is not a pass on today's.

**Evidence and its source**: Post a handoff and its check in the work thread. Within a run, a new handoff by the same author for the same work thread supersedes the previous revision, whether or not the work is a task. The author's handoff on another thread stays actionable. Supersession changes which review is current, not the historical receipt.

**Decision request**: An explicit question addressed to an actor, closed by an authorized **decision resolution**. A conversational reply can discuss the question without resolving it.

**Run receipt**: A summary of the run's handoffs, verification results, and outstanding decisions or linked work, not a watcher's proof of delivery. A completed task, a finished worker, a stopped run, and a verified revision are four different facts.
_Avoid_: An unqualified *done* when reporting one of these facts.

### Events and sync

**Event**: An ordered record of a change on the board. A **cursor** marks a position for a particular read or delivery stream; a board event cursor, an inbox acknowledgment, and a task-list page position are not interchangeable.

**Watcher**: A named, durable subscription to matching board events. Events can wait for a consumer while it is offline; a watcher is not a worker and does not launch an agent.

**Sync**: The connection between a Board thread and a conversation outside the board. A **source** names the mapping between an external location and a Board channel; a **connector** speaks to the provider. Importing a conversation is not permission to publish replies back.

**Sync inbox and outbox**: The sync inbox holds incoming provider work; the outbox holds changes awaiting external publication. Neither is a participant's swarm inbox.

## Workspace layout

| Path | Responsibility |
| --- | --- |
| [crates/board-core](crates/board-core) | SQLite storage and the shared domain: threads, tasks, plans, swarms, roles, access control, and events. |
| [crates/board-cli](crates/board-cli) | The `board` CLI, harness setup and skill installation, and local task and watcher runners. |
| [crates/board-mcp](crates/board-mcp) | Tool definitions, handlers, and the stdio transport behind `board mcp`. |
| [crates/board-api](crates/board-api) | The optional REST API and event stream behind `board serve`. |
| [crates/board-sync](crates/board-sync) | Connectors for GitHub, Slack, and Linear. |
| [ui](ui) | The native Rune desktop app, `board-ui`. |

See [Architecture](docs/INTEGRATIONS.md) for how these components run together.
