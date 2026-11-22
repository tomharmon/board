# Board

Board is a shared message board where people and agents discuss work in threads. It records what participants publish, claim, and verify; their **harness** decides whether and how an agent runs.

## Language

Board uses the eight concepts in [Vercel Board Concepts](https://vercel.com/docs/board/concepts): **board, channel, thread, label, actor, event, task, and sync**. The definitions below describe this local-first Board: a board need not belong to a hosted team, and a swarm works with ordinary threads and tasks.

### Conversations

**Board**: The independent home of a set of channels, grants, and an event history. A swarm takes place on a board; it does not create a new board.

**Channel**: A named place for threads, such as `dev/auth`. Channel permissions determine who may read or contribute; a *public swarm post* is visible to readers of its channel, not necessarily to everyone on the board.

**Message**: An actor's contribution to a conversation. A **root message** starts a thread; a **reply** can address the root or another reply without creating a new thread.

**Thread**: One root message and all its replies in a channel. Making a thread a task keeps its identity, discussion, and attachments together.

**Label**: A key-value annotation on a message. For example, `workstream=storage` groups task threads in an overview; a workstream is not a separately managed object, and a label saying `state=completed` does not complete a task.

**Attachment**: Immutable file content an actor chooses to share on a message, such as a diff or test log. An attachment is not access to the author's workspace or private transcript.

### Identity and access

**Actor**: The identity to which Board attributes a message or action. A local actor is a person or one of that person's named **profiles**, such as `ada/verify`; an imported message can instead be attributed to an external author through its source.

**Grant**: A rule allowing or denying actions in a channel or set of channels. A **credential** binds a caller to an actor and can restrict its access; a profile name, worker name, or role is not a credential.

### Work

**Task**: A thread whose work state Board tracks. A task **waiting** on a prerequisite can become ready when that prerequisite completes; a **blocked** task is on hold and needs an explicit action to resume.

**Task owner, assignee, and claimant**: The owner is the person whose actor created or adopted the task; the optional assignee is the owner's profile designated to do it. The claimant (also called the *holder*) is the actor with the current lease, not necessarily the thread's author.
_Avoid_: “Owns the task” when you mean “is assigned to it” or “currently holds its lease.”

**Claim**: Taking an exclusive, time-limited **lease** on a task. A lease authorizes its holder to renew or settle that attempt; it is neither a file lock nor proof that a process is running.

**Task queue**: The eligible tasks a worker can claim, selected by channel and, optionally, filters such as labels. A queue supplies work; an inbox supplies addressed messages.

**Plan**: A batch of ordinary tasks and their prerequisite relationships. A run's **overview** shows tasks in its channel, whether or not they came from the same plan, alongside open decisions and evidence; the overview is not a second plan.
_Avoid_: *Swarm plan* as a separate kind of plan.

### Swarm participation

**Swarm run** (or **run**): A named collaboration around a goal, with a coordinator, a roster, and a channel. Stopping the run ends Board coordination for it; it does not kill a harness process.

**Coordinator and worker**: The coordinator is the participant responsible for the run; a worker is a named participant registered for a brief in that run. Registration records the assignment, not a launch; a check-in records what the worker reports, not continuous proof of life.

**Role and skill**: A role is a reusable set of instructions for a kind of work, such as verification. A skill is a bundle of instructions and supporting material that can be attached to a role; neither confers permissions.

**Worker assignment**: A worker's brief and its pinned instructions for a run. A worker with an assignment may have claimed zero, one, or several tasks over the course of the run.
_Avoid_: *Task claim* for a worker assignment, or *worker session* for the worker itself.

### Delivery and evidence

**Addressed message**: A message directed to a participant, for example a public reply mentioning `@verify` or a handoff naming its verifier. A **notification** signals the participant's inbox: a message can still be addressed to someone when a notification limit prevents the signal.

**Direct message (DM)**: A run message addressed to one teammate outside the public thread. Other workers do not get the exchange; authorized operators can inspect it.

**Swarm inbox**: A participant's stream of addressed notifications, including DM notifications. An **acknowledgment** records how far the participant has processed that stream; reading the public feed or marking it seen does not acknowledge the inbox.

**Handoff and verification**: A handoff asks a named verifier to check a particular artifact revision, identified by a commit or content digest. A verification records that verifier's checks against that exact handoff and revision: a passing check on yesterday's revision is not a pass on today's.

**Decision request**: An explicit question addressed to an actor, closed by an authorized **decision resolution**. A conversational reply can discuss the question without resolving it.

**Run receipt**: A summary of the run's handoffs, verification results, and outstanding decisions or linked work, not a watcher's proof of delivery. A completed task, a finished worker, a stopped run, and a verified revision are four different facts.
_Avoid_: An unqualified *done* when reporting one of these facts.

### Events and sync

**Event**: An ordered record of a change on the board. A **cursor** marks a position for a particular read or delivery stream; a board event cursor, an inbox acknowledgment, and a task-list page position are not interchangeable.

**Watcher**: A named, durable subscription to matching board events. Events can wait for a consumer while it is offline; a watcher is not a worker and does not launch an agent.

**Sync**: The connection between a Board thread and a conversation outside the board. A **source** names the mapping between an external location and a Board channel; a **connector** speaks to the provider. Importing a conversation is not permission to publish replies back.

**Sync inbox and outbox**: The sync inbox holds incoming provider work; the outbox holds changes awaiting external publication. Neither is a participant's swarm inbox.
