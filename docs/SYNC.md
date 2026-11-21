# Optional connectors

Board stores conversations and their history locally. `board serve`,
`board worker`, the inspector and ordinary reads never call GitHub, Slack or
Linear. Network use is explicit: `sync pull`, `sync preview`, `sync send`,
the owner-confirmed Slack channel join, and explicitly requested Slack file fetches. Remote configuration uses a
repository, channel ID or team key, not a configurable token destination.

## Read first

```sh
export BOARD_DB="$HOME/.local/share/board/board.sqlite3"
board sources add bugs github BurntSushi/ripgrep work/github
board --json sync preview bugs 1          # fetch without writing to the DB
board sync pull bugs 1                    # import one public issue and its discussion
board sync find bugs 1                    # local root ID
board read work/github

board sources add chat slack C01234567 work/chat
board sources add tickets linear ENG work/tickets
# Supply your token privately through the process environment before a remote command.
board sync pull chat 1727000000.123456
board sync pull tickets ENG-123
board sync pull tickets --watch --interval 5m  # foreground; Ctrl-C to stop
```

Omit the thread argument to scan the configured repository/channel/team.
The connector follows provider pagination, persists normalized events before
advancing its scan position, and drains stored events locally. Re-fetching
does not repost them. Slack scans root history so replies to older roots can
be discovered. Huge scans can be slow or rate limited; an interrupted scan
does not advance the last successful checkpoint. A scoped source belongs to
its owner and requires read and sync rights in the mapped channel. A
deliberately narrowed credential cannot administer integrations. On-demand
remote preview and joining are trusted-owner operations.

For each source the environment variable is `BOARD_SYNC_<NAME>_TOKEN`
(upper case; hyphens become underscores). Alternatives are
`BOARD_GITHUB_TOKEN`, `BOARD_SLACK_TOKEN` and `BOARD_LINEAR_TOKEN`.
Public GitHub reads need no token. Slack takes a bearer token appropriate
to the channel type and read/write method. Linear accepts an API key or a
full `Bearer ...` value for OAuth. The provider secrets are not written to
the board, its backups, or config; there is no built-in OAuth or keychain UI.
Use your shell's secret manager. Do not store tokens in messages or inbox
payloads.

| Provider | Represented locally |
| --- | --- |
| GitHub | Issues, PRs, ordinary comments, reviews and individual inline replies. External IDs include the repository mapping, issue number and comment/review ID. PR state, review, head, tags and assignees become labels. [Issue API](https://docs.github.com/en/rest/issues/issues), [review comments](https://docs.github.com/en/rest/pulls/comments). |
| Slack | A configured channel and its timestamp-addressed conversations. Fetches root history and paginated threaded replies. Native saved message edits/deletions and reaction events can also be ingested offline. Joining is never automatic: `board sync join chat --confirm` checks that the mapped channel is public and non-shared before asking Slack to join. [Thread API and token limits](https://docs.slack.dev/reference/methods/conversations.replies/). |
| Linear | One team's issues by UUID or issue identifier, nested comments, issue state and properties. A canonical UUID remains linked when the human identifier changes. [GraphQL API](https://linear.app/developers/graphql). |

## Publish deliberately

```sh
board sources publishing bugs on
board reply "$(board sync find bugs 1)" 'A reply to share'
board inbox list bugs --outgoing          # still local; nothing sent yet
# board sync send bugs --limit 10         # actually writes to GitHub
board sources publishing bugs off
```

Enabling publishing only queues subsequent local changes in **linked**
threads. Use `sync start NAME LOCAL_ROOT` to deliberately queue a *new*
provider conversation and its current local replies. Use `sync link NAME
REMOTE_THREAD EXTERNAL_ID LOCAL_ID` to assert an existing copy, and
`sync backfill NAME REMOTE_THREAD` to explicitly queue old unshared replies.
Do not link an object you do not control for editing. Delivered messages
preserve their confirmed external ID/permalink; `sync linked NAME URL` and
`sync copies NAME LOCAL_ID` recover them. Inbound posts do not echo back.

After an ambiguous external write (including a crash after intent was
recorded), later outgoing work on the same thread waits. The board cannot
tell whether a remote service accepted a request before the reply was lost.
Inspect the remote service and `board inbox list NAME --outgoing`; then use
`board inbox resolve NAME ITEM delivered --external ID [--thread REMOTE_THREAD]`.
Use `retry` **only after verifying nothing was created**, or `skip`.
Unrelated known threads proceed independently. An uncertain *new* thread
also fences inbound echoes for the source until its remote ID is known.
Deleting a published GitHub/Linear root redacts its title/body rather than
deleting the issue and its discussion. Turning publishing off skips queued
unsent work; it cannot retract a write already in flight.

## Offline inputs

**Slack files and reactions.** Ordinary pulls do not copy private bytes. Use
`sync fetch-file NAME THREAD EXTERNAL_MESSAGE FILE_ID` to attach a specific
file to its imported message. Slack must confirm that exact channel and
message share it. Repeating the fetch returns the same immutable local asset.
`sync share-file NAME THREAD LOCAL_MESSAGE FILE_ID_OR_KEY` explicitly stages
one attached file for publishing. `sync send` uploads it without forwarding
your API token to the temporary upload endpoint; `sync files NAME LOCAL_MESSAGE`
lists confirmed copy IDs in both directions. Bytes and annotations share the
same local backup. This uses the [Slack upload protocol](https://docs.slack.dev/reference/methods/files.completeUploadExternal/).
With publishing enabled, a Slack reaction is added for the first local reactor
and removed for the last; inbound snapshots import reported users, and saved
events supply removals. Linear supports outbound adds on linked comments.

For a private local tool, make a `local` source and send a normalized envelope:

```sh
board sources add buildfeed local build-events work/build
board inbox push buildfeed - <<'JSON'
{"key":"build-19","import":{"thread":"B19","external":"root","author":"builder","change":{"op":"post","text":"Build 19 passed"}},"raw":{"build":19}}
JSON
board sync drain buildfeed
board sync find buildfeed B19 root
```

`sync import NAME FILE` performs the same write immediately. Supported
operations are `post`, `upsert`, `edit`, `delete`, `react`, `labels` and
`signal` (update a linked root and optionally post a reply only if its
label preconditions still match).
Include `parent` for a precise external reply target. A reused key with a
different normalized operation conflicts. Original payloads are visible to
the source owner in the inbox, not in ordinary thread reads.

Already-obtained provider event files can be enqueued without network:
`board sync ingest NAME KIND DELIVERY_ID FILE`, then `board sync drain NAME`.
Use a stable delivery ID. For GitHub, KIND is `issues`, `pull_request`,
`issue_comment`, `pull_request_review`, `pull_request_review_comment` or
`workflow_run`. A completed CI run changes its workflow's label only when
its commit still matches the linked PR head; a failure also posts a reply.
For Slack/Linear use `slack`/`linear`. These files are **trusted local
input**, not signed webhooks; do not relay arbitrary Internet traffic here.
For child-only events, import their parents first. Polling a list cannot by
itself prove a disappeared comment was deleted.

## Verification and limits

An anonymous, read-only pull and replay of [public issue #1](https://github.com/BurntSushi/ripgrep/issues/1)
was run. [Loopback HTTP fixtures](../crates/board-sync/src/tests.rs) exercise
real request construction, response parsing, pagination, parent mapping and
publishing without contacting private accounts. **No live Slack/Linear account
or external write was tested.** Those tests are not a provider certification.
The connectors mirror conversations, not repositories or every provider
feature: there is no public webhook endpoint, automatic remote identity
impersonation, or automatic mirroring of private file bytes. See the
[coverage inventory](PARITY-AUDIT.md).
