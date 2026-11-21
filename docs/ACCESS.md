# Access, files, and profiles

## Trust boundary

The SQLite file, its WAL, and backups are private to the owning OS account.
Opening the file normally is an owner operation, not authentication as a remote
person. An owner may use named profiles for attribution. A process that can
open or copy the database, change the binaries, or read the owner's environment
can bypass application permissions. **Give an untrusted process only a delegated
bearer and access to the loopback REST service, not the database or its master
token.** Separate OS accounts/filesystem permissions are the actual boundary.

No external identity service or network is involved. The loopback server's
session master (`BOARD_API_TOKEN`, or the one printed on startup) is distinct
from durable delegated credentials. Root keys, token hashes, and lease secrets
are not interchangeable. Don't put secrets in command arguments, shell history,
or checked-in files. The owner can still administer grants after revoking every
delegated grant.

## Issue a credential

```sh
export BOARD_DB="$HOME/.local/share/board/work.sqlite3"
board grants create teammate --can 'ops/=read,post;ops/private/=none'
board grants list
board grants explain teammate/helper ops/shift

# jq is used only to extract the one-time bearer from JSON.
issued=$(board --json credentials issue teammate/helper --scope 'ops/=all' --ttl 3600)
delegated=$(printf '%s' "$issued" | jq -er .token)
credential_id=$(printf '%s' "$issued" | jq -er .credential.id)

BOARD_TOKEN="$delegated" board whoami
BOARD_TOKEN="$delegated" board post ops/shift 'Shift began'
BOARD_TOKEN="$delegated" board read ops/

# Use in another shell with the same BOARD_DB, with NO BOARD_TOKEN:
board serve --port 4100
# With the delegated bearer in your original shell:
curl -fsS -H "Authorization: Bearer $delegated" http://127.0.0.1:4100/v1/channels

# After use (unbound owner shell):
board credentials revoke "$credential_id"
unset issued delegated credential_id
```

Creating a grant does not mint a bearer. Issuance stores a hash; the secret is
returned only once, never by a listing or event. TTL is optional, in seconds
(1 second to 365 days). Without a TTL the credential lives until revoked.
Credential actors are fixed: `BOARD_USER`, `--as`, and a request body cannot
impersonate another actor. A failed authentication does not fall back to the
owner. Revoke a grant with `board grants revoke ID`, or change it with
`board grants update ID SUBJECT --can '...'`. Reads and mutations reevaluate
the current grant in SQLite, without restarting the server or UI.

Subjects are an exact actor such as `teammate/helper`, a person such as
`teammate` (also matches their profiles), or everyone (`*`). Everyone rules
can constrain an admitted actor but cannot admit an unknown actor. Across
all applicable grants, the most specific channel pattern wins: exact name,
longest matching subtree, then wildcard. A subtree `ops/` includes `ops`
itself. At the winning pattern an exact actor outranks their person, who
outranks everyone. Only grants tied at both levels add permissions together.
An explicit `none` is a real rule, not a missing entry.
For a hard ceiling, issue a credential with `--scope`. An optional HTTP
`X-Board-Can: ops/=read;archive=read` further intersects, never expands, its
permissions. A narrowed master is also kept to the server's selected actor.

| Action | Allows |
| --- | --- |
| read | Discover/read permitted channels, messages, events, tasks, and their files. Hidden IDs answer 404, like nonexistent IDs. |
| post | Create roots; edit/delete one's own posts. Replies and explicit channel creation also require read. |
| react | React, plus read. |
| label | Change labels and work tasks, plus read. Labels on a new post also require label. |
| sync | Use an authorized source in its configured channel, plus read. Source ownership and integration restrictions still apply. |
| manage | All channel actions, including permitted administrative changes to work resources. |

Source management, raw inbox payloads, and delivery receipts require source
ownership plus read and sync at its channel; a credential-specific or per-request
narrowing cannot act as the integration. Reading the imported post or the
public actor card does not expose the receipt, platform thread key, or stored
raw input. Source results are filtered in the same transaction as ordinary
reads, including task dependencies pointing outside the reader's channels.

Credential administration, raw database backups, and unrestricted worker
maintenance remain owner operations. `manage` at the wildcard (`*`) permits
registering schemas and listing grants; it is not an OS-owner escape.
A write-only caller can make
a channel with its first post but cannot browse or reply to other messages.
Its keyed replay exposes its own originally blind post, not reactions, labels,
or task history subsequently added to it. Exact mentions of others are masked
in that replay. A post written with read access does not become a blind
writer's editable post after their read grant is removed.

The REST resources are `/v1/grants` (GET/POST), `/v1/grants/{id}` (GET/PUT/PATCH/DELETE),
`/v1/credentials` (GET/POST), `/v1/credentials/{id}` (GET/DELETE), and
`/v1/access?subject=teammate/helper&channel=ops/shift` (GET). A grant body is
`{"subject":"teammate","can":{"ops/":["read","post"],"ops/private/":[]}}`.
An issuance body is
`{"actor":"teammate/helper","scope":{"ops/":["read","post"]},"ttl":3600}`.
Global managers can explain any delegated subject's grants, including winning
patterns, grant IDs, and principal level. Other delegates can explain only
their own effective access, without other grants' details. Unknown/expired/revoked HTTP credentials
answer 401, a known credential with no matching grant answers 403.

Delegated managers may grant to others where they have authority. A change
(including removing a rule) requires manage at its old and new patterns; a
broad grant cannot evade a narrower limit on the manager's own authority.
Admitting a new subject cannot give them an everyone permission denied to the
maker. The transaction checks prospective permissions too: removing one's
ability to undo the change is refused unless explicitly requested with
`--lock-me-out` (REST: `lock_me_out=true`). This never removes OS-owner access.
One group grant can contain multiple subjects:

```sh
board grants create operator helper/ci --can 'ops/review/=read'
board grants patch GRANT_ID --add helper/report --remove operator
```

To install a policy set without exposing an intermediate state:

```sh
printf '%s\n' '[{"subject":"reader","can":{"ops/":["read"]}},{"subject":"writer","can":{"ops/":["read","post"]}}]' |
  board --json grants batch - --idempotency-key install-ops-1
board grants receipt BATCH_ID
```

`POST /v1/grant-batches` accepts `{"grants":[...]}` (1 to 16 entries) and an
optional `Idempotency-Key` header. Validation, all created grants, their events,
and one batch receipt commit or roll back together. First creation returns 201
with `Location: /v1/grant-batches/{id}`; GET that resource for the original
snapshot, even after a grant changes or is revoked. A retry from the same actor
with the same ordered grants returns 200 without granting anything again.
Reusing a batch key for a different actor or different entries returns 409.

## Immutable files

```sh
board post ops/shift 'Quick evidence' --attach ./small.log
upload=$(board --json upload ops/shift ./recording.webm --type video/webm)
key=$(printf '%s' "$upload" | jq -er .attachment.key)
board post ops/shift 'First run' --attachment-id "$key"
board post ops/shift 'Same recording, no duplicate bytes' --attachment-id "$key"
board download "$key" ./received.webm
```

The board records filename, MIME type, byte size, stable numeric ID, and a
random 128-bit hex key. Either ID or key works for message attachment
references and downloads. A message carries at most 10 files. Inline files and
simple uploads are limited to 4 MiB each. Dotfiles, paths, formatting controls
and invisible directional characters are rejected. Downloads are served as
attachments, sandboxed and with MIME sniffing disabled. A MIME type describes
the file; it does not establish its safety.

Larger files, up to 100 MiB, use resumable local SQLite uploads; there is no
signed external URL, object store, or upload service. The CLI streams a large
file in 4 MiB parts. For `post`, `reply`, `schedule` and new tasks,
`--attach` checks all file paths and sizes before staging; if any file is
large, it stages the batch in argument order. Staging and posting are distinct
operations: rejection of a later post publishes no message or placement, but
previously finished uploads remain private and reusable. A committed
idempotent post can be retried even after its original path is gone.
A direct REST client can persist its ticket and resume:

1. `POST /v1/uploads` with `{"channel":"ops/shift","name":"run.bin","size":6,"content_type":"application/octet-stream"}` and optionally a lowercase hex `sha256`. The response's `Location` identifies the ticket.
2. `PUT {Location}/parts/0` with raw bytes `abc`; `GET {Location}` returns `received: 3`. `PUT {Location}/parts/3` with `def`. Parts must arrive at the next offset; identical retries before completion are harmless, conflicting bytes are refused.
3. `PUT {Location}/completion` with no body. The server checks size and requested digest while copying stored parts into the final SQLite blob. An incomplete or mismatched upload publishes no file. Creation returns 201 with the attachment location; repeating completion returns 200 and the same attachment.
4. `POST /v1/messages` with `{"channel":"ops/shift","text":"Proof","attachment_ids":["<attachment key>"]}`. `GET /v1/attachments/{id-or-key}` returns the bytes. The simple alternative is `POST /v1/attachments?channel=ops%2Fshift&name=run.bin` with raw bytes and `Content-Type`.

Tickets are actor-bound and expire after one hour before completion. A later
start reclaims expired staging data. Up to eight unfinished tickets per actor,
800 MiB of declared unfinished uploads board-wide, and 512 parts per ticket.
Before a file is attached or downloaded the board checks that it is readable:
delegated read permission, even on every channel, needs an accessible live
placement, or the uploader's own permitted provenance at the original channel.
The trusted database owner can recover its contents. Knowing an ID alone is not a grant. Deleting
a message removes its placement, not the immutable bytes or other messages'
placements. An OS-owner backup includes files, private work state and
credential hashes; protect it accordingly.

## Directory and drafts

```sh
board --as helper profile --handle teammate/helper --name 'Shift helper' --avatar https://example.invalid/helper.png
board actors --details --query helper
```

`GET /v1/actors` returns IDs and their read-snapshot cursor for scripts. Add `details=true` for records, an
optional `query` (case-insensitive substring) and up to 100 exact `id` query parameters. `PUT /v1/profile`
accepts the caller's `handle`, `name`, and optional `avatar` (empty string clears,
omission leaves it unchanged). The CLI also supports repeated `actors --id ID`.
A local display name is presentation,
not a credential. A credential-authenticated profile update records a local
verification; external actors are attributed to their importing source, not
claimed as independently verified accounts. Avatar URLs, when present, are
metadata, not a requirement for offline operation. A master-authenticated REST
request can select its actor's profile with `X-Board-Profile: helper; avatar="https://example.invalid/helper.png"`.
The bearer of a delegated credential cannot use this to switch identities.
Named CLI and REST profiles use the same lowercase spelling. Avatar metadata
(HTTPS, or HTTP loopback) is not fetched; legacy separate avatar/session headers do not
establish profile identity.

In restricted sessions, the directory shows oneself and known actors who
authored, are mentioned by exact ID, or currently react in readable channels.
Unknown bracketed text doesn't register a new identity. Posting, editing, and
replying resolve unambiguous full handles from the writer's *visible* directory
to stable `@[actor-id]` text; in a thread, participants can break a tie.
Unresolved, partial or ambiguous handles and over-budget rewrites remain as
typed. Mutating responses include suggestions; durable events and reads do not
store those private candidate lists. Mention search ignores paired backticks
and typed data fences, normalizes ordinary handles, and preserves exact IDs.
`--by me` and `--mentions me` select only the caller, not their child profiles;
a named person without this exact selector includes their profiles.

Inspector text drafts and read checkpoints persist locally without creating board
events. They are bound to the actor and, for delegates, credential and narrowing
scope. Thread drafts recheck access to their thread; restoring a draft cannot
read an inaccessible reply target. Composer drafts include private staged file
bytes, routing and guards, so a restart does not require the original file
path. A stale window cannot send or clear a newer draft.

Schema upgrades run in a write transaction. The v2 key upgrade keeps the
earliest successful post for formerly ambiguous keys without deleting message
history. A v4 upgrade moves legacy inline bytes into immutable assets without
renumbering old attachment IDs. V5 rebuilds the prose mention index and adds
the directory and draft state. V7 refreshes that index for paired triple
backticks in prose. V8 converts one-subject grants into grouped grants and adds
local profile avatar metadata without changing historical grant receipts.
V12 builds indexed channel and thread summaries from existing messages and
adds source lookup indexes without creating new board events.
Existing custom schema definitions remain unchanged.
