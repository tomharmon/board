# Optional connectors

Board stores conversations and their history locally. `board serve`,
`board worker`, the inspector and ordinary reads never call GitHub, Slack or
Linear. Network use is explicit: `sync pull`, `sync preview`, `sync send`,
the owner-confirmed Slack channel join, status or file transfer. Remote configuration uses a
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

`sync preview NAME --limit 10 [--cursor NEXT]` reads one provider page and
returns its continuation without writing locally. On Slack, the owner can
preview or explicitly join another channel by ID, `'#name'` or permalink. All
on-demand Slack reads, including the configured channel, require a public,
non-shared channel that the app **already belongs to**. Previews can address
a precise reply as `C01234567:1727000000.123456`; read its enclosing thread
and pass its root and continuation to resume. Reading never joins or changes
a mapping. Explicit pulls of a configured source are separate from previews.
[Public channel listing](https://docs.slack.dev/reference/methods/conversations.list/)
and [channel membership](https://docs.slack.dev/reference/methods/conversations.info/)
are checked using the configured app, not a local display name.

GitHub previews also accept an issue or pull request URL in the configured
repository. A Linear preview accepts its configured team, an issue ID or URL,
or a project URL restricted to that team. Team and project pages return readable
summaries; comment pages return a continuation. Links supply identifiers,
not new API hosts or token destinations. GitHub and Linear tags keep the
provider's display value (for example, `tag.good-first-issue=Good First Issue`).
A sparse update does not delete a label family the provider did not supply.
To route new issues and pull requests in the same repository to different
Board channels, create a source for each mapping and set
`board sources configure bugs --github-items issues` and
`board sources configure reviews --github-items pulls` (default: `all`).
This applies to native intake and explicit pulls. Changing the route does
not disconnect an already linked thread.

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

By default, enabling publishing only queues subsequent changes in **linked**
threads. Use `sync start NAME LOCAL_ROOT` to deliberately queue a *new*
provider conversation and its current local replies. Use `sync link NAME
REMOTE_THREAD EXTERNAL_ID LOCAL_ID` to assert an existing copy, and
`sync backfill NAME REMOTE_THREAD` to explicitly queue old unshared replies.
Do not link an object you do not control for editing. Delivered messages
preserve their confirmed external ID/permalink; `sync linked NAME URL` and
`sync copies NAME LOCAL_ID` recover them. Inbound posts do not echo back
to their origin, but can be delivered to independent, explicitly enabled mirrors.

```sh
board sources configure chat --mirror-roots true   # new roots in the mapped channel
board sources publishing chat on                   # still requires an explicit sync send
board post work/chat "Just for the board" --no-sync
board sources configure chat --mirror-files true --display-authors true
```

Private roots exclude the entire thread, including edits, reactions, files,
manual linking and backfill. A private reply excludes just that message. The
outbox re-reads a live message before leasing: an edit while offline updates
the pending post, and an unsent tombstone does not get sent. Each switch is
stored per source. Optional author attribution is a visible line on a message
from the integration, not a post using a person's credentials.

GitHub never automatically opens issues from local roots. An owner may
explicitly request a new issue with `sync start`. To customize the body a
source publishes, put a Rhai string expression such as
`message.text + " [" + (message.labels.phase ?? "open") + "]"` in a file;
run `board sources renderer NAME --file PATH`. The input also includes the
channel, actor, current task state, files, root, message and destination.
The renderer never edits the Board message. Label changes only edit a
confirmed external copy when the rendered body actually changes. Clear it
with `board sources renderer NAME --clear`. See also [write rules](RULES.md)
for rejecting an import with an explicit, durable external response.

After an ambiguous external write (including a crash after intent was
recorded), later outgoing work on the same thread waits. The board cannot
tell whether a remote service accepted a request before the reply was lost.
Inspect the remote service and `board inbox list NAME --outgoing`; then use
`board inbox resolve NAME ITEM delivered --external ID [--thread REMOTE_THREAD]`.
Use `retry` **only after verifying nothing was created**, or `skip`.
Unrelated known threads proceed independently. An uncertain *new* thread
also fences inbound echoes for the source until its remote ID is known.
Deleting a published GitHub root redacts its title/body rather than deleting
the issue and its discussion. A Linear root edit updates the description
without replacing its external title, and deleting the local root leaves
the Linear issue intact. Turning publishing off skips queued
unsent work; it cannot retract a write already in flight.
Re-enabling publishing does not replay that skipped work automatically.
Use an explicit backfill to queue unshared live replies again. Turning off
file mirroring also cancels unsent file uploads; an explicit share can
restage a selected file. Neither switch retries ambiguous remote writes.

## Offline inputs

**Slack files and reactions.** Default pulls do not copy private bytes. Use
`sync fetch-file NAME THREAD EXTERNAL_MESSAGE FILE_ID` to attach a specific
file to its imported message. Slack must confirm that exact channel and
message share it. Repeating the fetch returns the same immutable local asset.
`sync share-file NAME THREAD LOCAL_MESSAGE FILE_ID_OR_KEY` explicitly stages
one attached file for publishing. `sync send` uploads it without forwarding
your API token to the temporary upload endpoint; `sync files NAME LOCAL_MESSAGE`
lists confirmed copy IDs in both directions. Bytes and annotations share the
same local backup. This uses the [Slack upload protocol](https://docs.slack.dev/reference/methods/files.completeUploadExternal/).
With `mirror_files` explicitly enabled, pulls also copy newly imported files
(up to 25 MiB per file and ten per message). Unavailable files do not block
the conversation. Failed downloads retry on later pulls up to three times;
`sync file-failures NAME` lists them and `file_notices` in the message
explains what didn't come over. A manual fetch can still recover a file.
For outbound Slack files, each upload is a separate job after the text
receipt, and deleting the Board message queues removal of confirmed copies.
A definite upload-permission refusal adds a file reference to the text copy;
an uncertain result still requires reconciliation. GitHub and Linear receive
filenames, sizes and the Board message ID, never attachment bytes.

Slack rich-text conversion supports code, emphasis, links and source-qualified
mentions; untrusted raw Slack control tags in a local message are escaped.
With `display_authors`, an explicit Slack read may look up names and avatar
metadata (`users.info`). It never downloads avatars. Owners may explicitly
call `sync status NAME THREAD 'Working'` (empty status clears it); this uses
[Slack's assistant status API](https://docs.slack.dev/reference/methods/assistant.threads.setStatus/)
and requires the correct provider-side scope and conversation type.
With publishing enabled, a Slack reaction is added for the first local reactor
and removed for the last; inbound snapshots import reported users, and saved
events supply removals. Linear supports outbound adds on linked comments.

The CLI and Rune show stored quotes of recognized message links. A Board copy
is only quoted inside the same Board channel; a blind writer gets no snapshot.
Ordinary posts never fetch URLs. A Slack pull may use an unfurl already
attached to the input or read the target of a Slack permalink in a verified
public non-shared channel of which the app is already a member. Private and
unavailable links stay unresolved; later richer input can fill them in.

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
operations are `post`, `upsert`, `reconcile` (partial label families), `edit`, `delete`, `react`, `labels` and
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
For Slack/Linear use `slack`/`linear`. Unsigned files are **trusted local
input**. To authenticate original bytes, set
`BOARD_SYNC_<NAME>_SIGNING_SECRET` and pass `--signature SIGNATURE`
(plus `--timestamp SECONDS` for Slack). Once the secret is set, the native
intake command refuses unsigned input. [GitHub](https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries),
[Slack](https://docs.slack.dev/authentication/verifying-requests-from-slack/)
and [Linear](https://linear.app/developers/webhooks) signatures are checked
before normalization; Slack and Linear also have freshness windows.
This is verification of locally supplied bytes, **not a public listener**.
An early GitHub comment can seed an unknown root from its included issue or
PR snapshot. Unrelated CI runs or a push alone do not create threads. For
child-only payloads without the parent data, pull/import their parents first.
Polling a list cannot by
itself prove a disappeared comment was deleted.

## Verification and limits

An anonymous, read-only pull and replay of [public issue #1](https://github.com/BurntSushi/ripgrep/issues/1)
was run. [Loopback HTTP fixtures](../crates/board-sync/src/tests.rs) exercise
real request construction, response parsing, pagination, parent mapping and
publishing without contacting private accounts. **No live Slack/Linear account
or external write was tested.** Those tests are not a provider certification.
The connectors mirror conversations, not repositories or every provider
feature: there is no public webhook endpoint or remote identity impersonation.
File sharing, source fan-out, native signatures, and rich text have
[additional protocol fixtures](../crates/board-sync/src/tests/coverage.rs). See the
[coverage inventory](PARITY-AUDIT.md).
