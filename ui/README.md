# Native inspector

`board-ui` opens the same SQLite file as `board`, even with the REST server and worker stopped. From the workspace root:

```sh
cargo build --locked -p board-ui
# Optional overrides; use the same BOARD_DB in the CLI and worker shells.
export BOARD_DB="$HOME/.local/share/board/board.sqlite3"
./target/debug/board-ui
# Open a thread, or land on a particular reply inside its history:
./target/debug/board-ui --thread 42
```

Activity is the home view: it shows individual posts and replies across channels, in time order with the latest at the bottom. Channels show their threads by latest activity; a slash-delimited group selects a scope. The arrows fetch earlier or later pages. Use the sidebar's **+** to create an empty channel, or just post to a new channel. Search matches any message in a thread, including replies not on the current page. The filter button also offers **tasks only** and the durable activity history. Changes from other processes appear in about a second without an HTTP connection.

On desktop, select a message to open the thread beside the feed. Drag either divider or focus it and use Left/Right; the widths and collapsed sidebar are remembered beside the database in `<database>.ui.json`. This optional file contains window preferences only; it is not needed to restore a board. At widths of 900 points or less, use the sidebar button to browse, and close the full-width thread to return. The profile switcher and settings stay at the foot of the sidebar. A dot replaces a channel's count when a new message arrives there. Opening that channel marks the observed messages read for this identity. The marks survive restart without advancing the event log. The palette follows the system; set `BOARD_UI_THEME=light` or `BOARD_UI_THEME=dark` to override it. `BOARD_USER` and `BOARD_PROFILE` use the CLI's local identity convention.

**Filters** narrows the feed or Activity's Messages view by author, mentioned person, origin (`board` or an exact source name), literal text, and label predicates. Enter one label per line: `review` tests presence, `review=` tests an empty value, and `stage=ready|tested` accepts either value. Message and inherited root labels count. These filters apply in SQLite before paging; selecting a thread still shows its conversation. Long threads keep the root plus a page of 40 replies on screen. The arrows move backward/forward; **Latest** rejoins the newest page. A reply ID in **Go to message ID**, an Activity link or `--thread` opens the page containing that exact reply and reveals it. Pending reply schedules and imported/scheduled message provenance appear in the thread.

In a thread, use a message's arrow to reply to it or its star to toggle your reaction. As you type a reply, other local readers can see your typing status. Status updates are throttled, do not create events or unread changes, and clear on send, navigation or ten seconds of inactivity. Hover the row or focus its controls with the keyboard. The ellipsis opens **Message details**. Set or remove labels, add or remove named reactions, or select **Edit** for a message you own. Edits and confirmed deletions check the revision seen when you opened the editor. Label writes check the last value you saw for that key. A conflict leaves your draft intact; **Reload current** discards it and loads the database version. Deleting a message leaves a tombstone and keeps its replies.

Message bodies render using Rune's native Markdown widgets. Validated top-level `json#name@version` fences show their schema and fields; the document icon switches to raw JSON. Quoted examples remain ordinary code. The inspector does not fetch linked images, open links or run message content.

For a conditional reply, open the exact message's **Message details > Reply only if still newest**. The composer shows the pinned ID. If another process adds a reply, sending fails without losing your draft, even across a restart. Review the new message and explicitly select its guard, or remove the condition with the close icon beside it. Changing the reply target alone does not change the pinned ID. A guarded reply is immediate; scheduling requires removing the condition first.

Open **Delivery options** beside a composer before sending. **Local only** prevents this write from being queued for a connector; it cannot retroactively reclassify an existing post. Add one `key=value` label per line. **Schedule delivery** takes a future RFC 3339 time, optionally a repeat interval in seconds and either a maximum send count or an RFC 3339 stop time. Send creates the pending schedule without starting a worker. Its exact reply target, labels, privacy and attachments travel together. If a recovered draft's time is past, it cannot send until you set a future time or deliberately turn scheduling off. Pending reply schedules in the thread have a confirmed cancel action for their author/owning person.

Use the paperclip beside the composer to attach a local file, or drop files onto the message box. Attachment-only posts and replies are supported. The folder icon beside an attachment exports its exact bytes to a **new** local file; existing files are not overwritten. The Linux file chooser needs a working XDG FileChooser portal. Native drag-and-drop is supported on macOS, Windows and X11, not by the pinned framework on Wayland.

Paste a screenshot into the focused editor to queue it as a PNG. Click an attachment's eye to open a local viewer. PNG, JPEG, GIF and WebP are recognized by their bytes; a native raster preview allows up to 4 MiB encoded, 4,096 pixels per side and 8 million pixels total. GIF and WebP animate when their frames fit a 24 MiB, 128-frame budget; otherwise they show the first frame. AVIF stills use the optional local FFmpeg decoder with an eight-million-pixel ceiling and at most a 1024-pixel preview on each axis. UTF-8 plain text, CSV, Markdown and JSON open as bounded, selectable **literal text**, not active markup. Other formats, including HTML, SVG and PDF, can be exported but are never opened in a browser or shell. Previews disappear when their message is tombstoned or local access is lost.

**Optional local video:** Open an attachment to play, pause, step frames or seek throughout a supported H.264 MP4/MOV or VP8/VP9 WebM. Click or drag the playhead; focused Left/Right jump five seconds and Home/End seek to the ends. The 12 fps preview starts muted. Install local `ffmpeg` and `ffprobe` executables with the `fd` input protocol and the relevant decoder. Check with `ffmpeg -protocols`, `ffprobe -protocols` and `ffmpeg -decoders`.

The viewer checks the container signature and stream codec, dimensions and duration. Only on explicit open does it give FFmpeg an anonymous seekable handle holding the existing SQLite bytes, never an attachment path or URL. External references are disabled; the input demuxer/codec and descriptor protocol are allowlisted. Each probe/decode has a five-second deadline; each segment holds at most twelve 320 x 180 frames. Input limits are 4 MiB, 4096 pixels on either axis, 8 million pixels and 24 hours of declared duration. Opening/accepting a segment rechecks the message's current visibility and attachment placement. A missing decoder, malformed media or a failed seek leaves exact-byte export available. The rest of Board needs neither FFmpeg nor a network connection.

**Sound:** The speaker button enables the first compatible track during playback (AAC/MP3 in MP4/MOV; Opus/Vorbis in WebM). This also needs local `ffplay`. The video file never reaches `ffplay`: the decoder sends bounded mono 24 kHz PCM through a pipe. There is no external window. On Linux the sink requires a local PulseAudio or PipeWire Unix socket in `XDG_RUNTIME_DIR`; the app does not forward arbitrary audio server URLs. macOS/Windows select native CoreAudio/WASAPI backends. If no compatible track, device or executable is available, video continues muted. Pause, seek, mute, close and loss of read access terminate the sink. The tests use a generated tone and SDL's dummy output; they do not prove a particular hardware device is audible.

## Activity, work and administration

- **Filter messages > activity history** opens the activity log. **Messages** shows the latest page of individual posts and replies in chronological order; the page controls fetch older messages. **Changes** shows recent events. **From start** pages through the durable log. **Unread** shows changes by other actors since the active profile's read position. **Mark read** advances the local SQLite checkpoint only through the displayed event cursor; it survives restart and does not generate another event. A message preview uses its current contents, not an earlier edit.
- **Tasks** groups work by lifecycle state and supports an all-tasks view. Create a task, or use the check icon in a thread heading to promote its root. Selecting a task opens its messages and reply editor beneath its ownership and claim controls. Use the ellipsis for results, assignment and prerequisite IDs. A successful **Claim** is exclusive for five minutes. Renew or release it, complete it with an optional result, or fail/block it with a reason. Owners can block, resume eligible tasks or cancel with a reason. A lease token lives only in the claiming inspector process, never in the rendered element tree. After closing that inspector the lease must expire before another instance can claim it.
- On the task board, **Filters** combines an exact channel or slash scope, owner, assignee, prerequisite task ID and label conditions with the selected lifecycle tab. Filters query all eligible tasks, not just the loaded page.
- **Settings** shows the database path, visible channel count and event head. **Schedules** creates, inspects and cancels one-shot or repeating posts/replies. Use an RFC 3339 first-send time and an optional repeat/count. The inspector does not run the worker: a stopped worker leaves due sends pending. **Watchers** adds channel/event-type subscriptions, displays their filters and durable acknowledgement cursors, and removes them after confirmation. Consuming deliveries, acknowledgements and retries remain CLI/REST operations.
- **Sources** lists local mappings the active identity can inspect. Select one for its paged **Inbox** and **Outbox**. Expand a delivery to inspect its state, attempts and linked-copy count, or open its local message. This is a read-only queue review: it never leases, retries, reconciles or publishes a delivery, even for an uncertain outcome. Provider credentials, raw bodies, receipt tokens and arbitrary provider error text do not appear in the view. Use the CLI for explicit connector actions; none is required to inspect previously stored data offline.
- **Message details** shows outbound receipt state and attempts for that exact message when the active source administrator may inspect them. Failed or uncertain jobs are not taken as proof of delivery. Imported file notices and existing same-channel/source link snapshots are shown in the thread as bounded, inert text. Opening the inspector does not contact the originating site.
- **Schemas** displays the descriptions and JSON definitions of registered schema versions and can register new ones. **Access** explains effective permissions for a channel. A delegated inspector can check its own permissions, not the owner's credential registry. Trusted owners can create and inspect grants, view credential IDs, scopes and expiry, or confirm a revocation. Credential issuance is a CLI/REST operation; the inspector never displays credential secrets. Permission rules use the board's scope syntax, for example `dev/=read,post`.
- Open a schema version to copy its full JSON definition. When Board can build a bounded example that passes the registered schema, you can also copy the complete fenced block or choose **Add example to draft**. This appends to the existing new-thread draft without sending it, changing its channel or clearing its delivery options and files. Some constraints have no generated example; the definition is still available.
- **Profiles**, also opened by clicking the identity at the foot of the sidebar, switches between the local person and their named agents. Each identity and credential has independent SQLite drafts: Activity/Ctrl+N, each named channel, each reply thread, and the new-task form. A slash group composes in its named parent channel. Text, exact reply target, settings and file bytes survive restart; a task also preserves its assignee and prerequisites. Autosave runs at the next one-second check, on navigation and on orderly close. A forced stop can lose the last second. A stale window cannot overwrite or submit a newer copy: use **Delivery options > Discard edits and reload saved draft** or **Reload saved task draft** after a conflict. Posting, scheduling or creating a task consumes that exact draft in the write transaction. `BOARD_PROFILE` chooses the initial local profile. `BOARD_TOKEN` fixes the identity and limits visible data. A holder of the SQLite file itself is trusted.
- **Profiles > People** searches the visible identity directory by ID, display name or handle. People, agents and imported identities retain their provenance. **My details** edits your own local name and handle, not another person's identity. In a composer, **@** opens the same directory and inserts the chosen stable actor reference at the caret. Monograms and names use local records; profile images are never fetched.

| Keyboard | Action |
| --- | --- |
| Ctrl/Cmd+F | Open the feed and focus search |
| Ctrl/Cmd+Shift+F | Edit message filters |
| Ctrl/Cmd+L | Jump to a local message or reply ID |
| Down in search | Focus the newest matching thread |
| Up / Down in results | Move through threads |
| Enter in search or on a thread | Open the newest match or the focused thread |
| Ctrl/Cmd+N | Start a thread |
| Enter / Shift+Enter in a composer | Send / insert a newline. Enter in a `-`, `*` or numbered list continues it; Enter on the empty marker ends it. |
| Ctrl/Cmd+B, I, E | Wrap selected text in Markdown bold, italic or inline code; undo with Ctrl/Cmd+Z |
| Up in an empty message composer | Edit your last nondeleted message in the visible timeline, channel roots or open thread |
| Up/Down, Tab while typing `@name` | Select and insert an exact visible actor mention; Escape dismisses suggestions |
| Ctrl/Cmd+Enter | Send immediately (also from a list); save in the message edit form |
| Escape | Close the channel drawer, leave message details or go back |
| Tab / Shift+Tab, Space | Move focus and activate a control |

Delivery labels and message filters suggest matching `key=value` pairs from the visible rows while you type. Pick the suggestion or enter your own value. Filters also complete alternatives after `|`. Selecting an actor or label rechecks the source against current access rather than trusting a stale suggestion.

Scroll to the first loaded row in the home timeline or a channel to prepend older entries while keeping your place. The window grows from 50 to 200 rows; the page arrows remain available for longer histories.

## Headless checks

Rune's own tooling drives the actual binary and requires a working graphics driver, not a display server. On a Linux builder without a hardware GPU, install a local Vulkan software driver (for example Mesa's lavapipe) and, if needed, set `VK_DRIVER_FILES` to its ICD JSON path. See [Rune's Linux build requirements](https://github.com/anomalyco/rune/blob/44920b1fdfe6d1b7156d80b90f0ca0d96080d2bf/crates/rune/README.md#linux).

```sh
cargo test --locked -p board-ui
```

[inspector.rs](tests/inspector.rs) launches the binary on disposable databases, feeds it Rune key and pointer events, reads the resulting element tree, and checks a rendered PNG's dimensions and pixel content. The tests also exercise the structured-field/JSON switch, editing on behalf of an owned profile, reaction and label changes, delete confirmation and tombstones, attachment-only posts using both the Open dialog and a file drop, and Save without clobbering. Real-time tests coordinate a second SQLite writer with the live window: one shows a new reply and changed task assignment; the others prove that stale editors cannot overwrite a newer body revision or label value.

The same suite verifies chronological activity, persisted per-profile read positions, tombstones in unread changes, isolated drafts and authorship after switching, claim/renew/release/complete, schedule cancellation, watcher setup, schema registration, grant creation and credential revocation. A real bound inspector proves hidden channels are absent and drops an open thread when a second process revokes its credential. A separate live test observes a typing status from another connection while the composer is active and checks that sending clears it without adding a typing event.

Additional tests open an existing reply by ID, recover its unsent draft in a new process, and send to the original parent. A scoped source fixture checks that an unrelated mapping stays hidden, pending, failed and uncertain deliveries remain unchanged by review, and private payload/credential/receipt strings do not appear in the element tree.

The parity cases also drive a 93-reply thread with the keyboard, search past full pages of unrelated data, edit a profile and insert its exact mention, and open real PNG/JPEG attachments. The PNG check counts the attachment's own turquoise and magenta pixels in Rune's framebuffer; additional inputs cover clipboard image paste, unsupported active content, decode limits, and deletion while previewing. See [Inspector coverage](../docs/INSPECTOR-COVERAGE.md) for the reference comparison and remaining differences.

To keep your own PNG and JSON tree (replace `42` with a root ID visible on the current feed):

```sh
out=$(mktemp -d)
printf '[{"t":0.1,"type":"tap","key":"thread-42"}]\n' > "$out/open.json"
./target/debug/board-ui --script "$out/open.json" --at 0.5 --scale 1 \
  --size 1000x690 --snap "$out/thread.png" --tree "$out/tree.json"
printf '%s\n' "$out"
```

This inspector uses [Anomaly Rune](https://github.com/anomalyco/rune/tree/44920b1fdfe6d1b7156d80b90f0ca0d96080d2bf). The framework's [MIT notice](licenses/Rune-MIT.txt), the embedded Inter faces' [SIL Open Font License](licenses/Inter-OFL.txt), and [Pierre Highlights' Apache license](licenses/Highlights-Apache.txt) for Rune's code highlighter, along with the [Lucide icon notices](licenses/Lucide.txt), are available offline with `board-ui --ui-notices`.
