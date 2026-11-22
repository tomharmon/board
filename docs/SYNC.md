# External connectors

Board includes optional connectors for **GitHub**, **Slack**, **Linear**, and **Local** event feeds. Everything stays offline by default—only explicit `board sync` commands make network requests.

## How connectors work

A **source** maps an external destination (a GitHub `owner/repo`, Slack channel ID, or Linear team key) to a local Board channel:

```sh
board sources add bugs github my-org/my-repo work/github
board sources add tickets linear ENG work/tickets
board sources add team slack C01234567 work/slack
```

You can also create and inspect sources offline in Rune under **Settings > Sync**.

Provider tokens are read only from your shell environment (`BOARD_SYNC_<NAME>_TOKEN`, or fallback `BOARD_GITHUB_TOKEN`, `BOARD_SLACK_TOKEN`, `BOARD_LINEAR_TOKEN`). Secrets are never saved to SQLite or backups.

## Pulling work into Board

### Sync conversations into channels

```sh
board sync preview bugs 42          # inspect without writing locally
board sync pull bugs 42             # import issue/PR #42 and its discussion
board sync pull tickets ENG-123     # import a Linear issue and its comments
board sync pull tickets --watch --interval 5m
```

Omit the thread ID to scan the configured repository, channel, or team. Connectors paginate cleanly, checkpoint their scan position, and deduplicate replays so re-pulling never duplicates messages.

### Pull tickets as claimable tasks (for agent swarms)

To feed a backlog of GitHub or Linear issues directly into a swarm, pull them as tasks:

```sh
board sync pull tickets --as-tasks
board sources configure bugs --github-items issues
board sync pull bugs --as-tasks
```

Only thread roots newly written during the pull become tasks. That includes queued roots left by an interrupted earlier pull, even when the provider returns nothing new. Their provider metadata is preserved, and the local actor who requested promotion owns the task, not the remote issue author. Comments stay replies. Replaying a pull does not promote previously imported ordinary threads, reopen completed tasks, or replace live claims. The request is saved with the inbox item: an offline `board sync drain bugs` after a restart still commits the root, task, and receipt together. A delegated puller needs read, sync, and task permission in the mapped channel; a revoked credential cannot promote queued roots.

Workers can claim tickets exclusively via `board tasks claim ID` and complete them using their lease. Pulling a network source is an explicit provider read, not part of offline drain or Board startup.

## Supported providers

| Provider | What is synced |
| --- | --- |
| **GitHub** | Issues, PRs, comments, reviews, inline review replies, and provider fields (state, review status, head, tags, assignees). Can filter to `issues`, `pulls`, or `all`. |
| **Linear** | Team issues (tracked by canonical UUID even if the human key changes), nested comments, issue state, and labels. |
| **Slack** | Channel history, threaded replies, reactions, rich-text formatting, optional opt-in file mirroring (`--mirror-files`), and assistant thread status. |
| **Local** | Normalized JSON event envelopes (`board sync import` / `board inbox push`) for integrating custom local scripts or CI pipelines offline. |

## Publishing back (opt-in)

Publishing is **off by default** on every source. Nothing leaves Board until you explicitly enable publishing and run `board sync send`:

```sh
board sources publishing bugs on
board reply "$(board sync find bugs 42)" "Fixed and verified in PR #98"
board sync send bugs
```

- **Keep internal chatter private:** Pass `--no-sync` on any post or reply to keep it strictly local. A `--no-sync` root keeps its entire thread off external services.
- **Starting vs. joining remote threads:** Use `board post CHANNEL "..." --sync SOURCE` to start a new external thread, or `--sync SOURCE:THREAD` / `board sync join-thread` to attach future replies to an existing remote thread without exporting older local history.
- **Outbox & safe recovery:** Outbound writes sit in a durable per-thread outbox. If a network error leaves an external write uncertain, Board pauses that thread's outbound lane rather than blindly double-posting, letting you inspect and reconcile via `board inbox list` and `board inbox resolve`.
- **Files:** Slack supports explicit file fetch/share (`sync fetch-file`, `sync share-file`) or automatic file mirroring when enabled; GitHub and Linear receive clean file summaries rather than binary blobs.
