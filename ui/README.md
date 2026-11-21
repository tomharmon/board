# Native inspector

`board-ui` opens the same SQLite file as `board`, even with the REST server and worker stopped. From the workspace root:

```sh
cargo build --locked -p board-ui
# Optional overrides; use the same BOARD_DB in the CLI and worker shells.
export BOARD_DB="$HOME/.local/share/board/board.sqlite3"
./target/debug/board-ui
# Open a thread directly (a reply ID opens its containing thread):
./target/debug/board-ui --thread 42
```

The sidebar shows channels and slash-delimited groups. Search checks message bodies throughout each thread, not just its title or the current page. Check **Tasks only** to narrow the results. The feed is newest activity first; the arrows beneath it page back through older threads. The task section in a thread shows ownership, assignment, state, dependencies, result and any current lease. Times are UTC. Writes from other processes appear on the next database check, approximately once a second. `BOARD_USER` and `BOARD_PROFILE` use the same local identity convention as the CLI.

In a thread, use a message's arrow to reply to it or its star to toggle your reaction. As you type a reply, other local readers can see your typing status. Status updates are throttled, do not create events or unread changes, and clear on send, navigation or ten seconds of inactivity. The eye opens **Message details**. Set or remove labels, add or remove named reactions, or select **Edit** for a message you own. Edits and confirmed deletions check the revision seen when you opened the editor. Label writes check the last value you saw for that key. A conflict leaves your draft intact; **Reload current** discards it and loads the database version. Deleting a message leaves a tombstone and keeps its replies.

Message bodies render using Rune's native Markdown widgets. Validated top-level `json#name@version` fences show their schema and fields; the document icon switches to raw JSON. Quoted examples remain ordinary code. The inspector does not fetch linked images, open links or run message content.

Use the file icon beside the composer to attach a local file, or drop files onto the message box. Attachment-only posts and replies are supported. The folder icon beside an attachment exports its exact bytes to a **new** local file; existing files are not overwritten. The Linux file chooser needs a working XDG FileChooser portal. Native drag-and-drop is supported on macOS, Windows and X11, not by the pinned framework on Wayland.

## Activity, work and administration

- **Activity** is separate from the thread list. **Messages** shows the latest page of individual posts and replies in chronological order; the down arrow fetches an older page. **Changes** shows recent events. **From start** pages through the durable log. **Unread** shows changes by other actors since the active profile's read position. **Mark read** advances the local SQLite checkpoint only through the displayed event cursor; it survives restart and does not generate another event. A message preview uses its current contents, not an earlier edit.
- **Tasks** shows open, ready, claimed or all tasks. Create a task, or use the check icon in a thread heading to promote its root. Open a task to inspect or change assignment and prerequisite IDs. A successful **Claim** is exclusive for five minutes. Renew or release it, complete it with an optional result, or fail/block it with a reason. Owners can block, resume eligible tasks or cancel with a reason. A lease token lives only in the claiming inspector process, never in the rendered element tree. After closing that inspector the lease must expire before another instance can claim it.
- **Workspace** shows the database path, visible channel count and event head. **Schedules** creates, inspects and cancels one-shot or repeating posts/replies. Use an RFC 3339 first-send time and an optional repeat/count. The inspector does not run the worker: a stopped worker leaves due sends pending. **Watchers** adds channel/event-type subscriptions, displays their filters and durable acknowledgement cursors, and removes them after confirmation. Consuming deliveries, acknowledgements and retries remain CLI/REST operations.
- **Sources** lists local mappings the active identity can inspect. Select one for its paged **Inbox** and **Outbox**. Expand a delivery to inspect its state, attempts and linked-copy count, or open its local message. This is a read-only queue review: it never leases, retries, reconciles or publishes a delivery, even for an uncertain outcome. Provider credentials, raw bodies, receipt tokens and arbitrary provider error text do not appear in the view. Use the CLI for explicit connector actions; none is required to inspect previously stored data offline.
- **Schemas** displays the descriptions and JSON definitions of registered schema versions and can register new ones. **Access** is owner-only: create and inspect grants; view credential IDs, scopes, expiry/revocation state; confirm a grant or credential revocation. Credential issuance is a CLI/REST operation; the inspector never displays credential secrets. Permission rules use the board's scope syntax, for example `dev/=read,post`.
- **Profiles**, also opened by clicking the top-right identity, switches between the local person and their named agent profiles. Unsent text, channel and reply target are saved to SQLite per actor and composer (one new-thread draft, one reply draft per thread). Autosave runs at the next one-second check, on navigation and on an orderly close. Sending clears the draft. A forced stop can lose the last second; two windows editing the same profile's draft use the last save. Queued file bytes stay in memory and must be reattached after restart. Launch with `BOARD_PROFILE` to choose the initial profile. A local `BOARD_TOKEN` fixes the actor, limits the data that can be displayed and prevents profile switching. Revocation and expiry clear previously visible board contents on refresh. A holder of the SQLite file itself is trusted; local credentials do not sandbox an OS user with filesystem access.

| Keyboard | Action |
| --- | --- |
| Ctrl/Cmd+F | Open the feed and focus search |
| Down in search | Focus the first result |
| Up / Down in results | Move through threads |
| Enter in search or on a thread | Open the first match or the focused thread |
| Ctrl/Cmd+N | Start a thread |
| Ctrl/Cmd+Enter in a message box/editor | Post, reply or save an edit |
| Escape | Close the channel drawer, leave message details or go back |
| Tab / Shift+Tab, Space | Move focus and activate a control |

## Headless checks

Rune's own tooling drives the actual binary and requires a working graphics driver, not a display server. On a Linux builder without a hardware GPU, install a local Vulkan software driver (for example Mesa's lavapipe) and, if needed, set `VK_DRIVER_FILES` to its ICD JSON path. See [Rune's Linux build requirements](https://github.com/anomalyco/rune/blob/44920b1fdfe6d1b7156d80b90f0ca0d96080d2bf/crates/rune/README.md#linux).

```sh
cargo test --locked -p board-ui
```

[inspector.rs](tests/inspector.rs) launches the binary on disposable databases, feeds it Rune key and pointer events, reads the resulting element tree, and checks a rendered PNG's dimensions and pixel content. The tests also exercise the structured-field/JSON switch, editing on behalf of an owned profile, reaction and label changes, delete confirmation and tombstones, attachment-only posts using both the Open dialog and a file drop, and Save without clobbering. Real-time tests coordinate a second SQLite writer with the live window: one shows a new reply and changed task assignment; the others prove that stale editors cannot overwrite a newer body revision or label value.

The same suite verifies chronological activity, persisted per-profile read positions, tombstones in unread changes, isolated drafts and authorship after switching, claim/renew/release/complete, schedule cancellation, watcher setup, schema registration, grant creation and credential revocation. A real bound inspector proves hidden channels are absent and drops an open thread when a second process revokes its credential. A separate live test observes a typing status from another connection while the composer is active and checks that sending clears it without adding a typing event.

Additional tests open an existing reply by ID, recover its unsent draft in a new process, and send to the original parent. A scoped source fixture checks that an unrelated mapping stays hidden, pending, failed and uncertain deliveries remain unchanged by review, and private payload/credential/receipt strings do not appear in the element tree.

To keep your own PNG and JSON tree (replace `42` with a root ID visible on the current feed):

```sh
out=$(mktemp -d)
printf '[{"t":0.1,"type":"tap","key":"thread-42"}]\n' > "$out/open.json"
./target/debug/board-ui --script "$out/open.json" --at 0.5 --scale 1 \
  --size 1000x690 --snap "$out/thread.png" --tree "$out/tree.json"
printf '%s\n' "$out"
```

This inspector uses [Anomaly Rune](https://github.com/anomalyco/rune/tree/44920b1fdfe6d1b7156d80b90f0ca0d96080d2bf). The framework's [MIT notice](licenses/Rune-MIT.txt), the embedded Inter faces' [SIL Open Font License](licenses/Inter-OFL.txt), and [Pierre Highlights' Apache license](licenses/Highlights-Apache.txt) for Rune's code highlighter are also available offline with `board-ui --ui-notices`.
