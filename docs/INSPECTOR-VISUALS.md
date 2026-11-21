# Inspector visual comparison

The target was the local inspector in reference revision
`c2d3324b117adaa8e9a0107e15ea2fca207c95ac`, not a screenshot inferred from its README.
We built its production inspector in a disposable directory, started its local backend
and inspected the rendered page in Chromium. The comparison board had three channels,
four root posts, one reply, a label and a reaction. We rendered the same conversations
through the Rust CLI and the actual Rune binary. The screenshots in this repository
contain only the native application; no browser bundle, web fonts or reference
screenshots are shipped.

The five native captures and element trees check the resting layout against
measured browser positions, not byte-for-byte rendering. The native palette
and input surfaces intentionally use stronger contrast than the reference.

We also exercised non-resting behavior in the reference Chromium app: a reply-only
filter on Home, the same filter in a channel, and a new message arriving while
scrolled up. [Rune tests](../ui/tests/inspector/reading.rs) check the corresponding
rows, track the old row's position within two points, and place the new row onscreen
after the jump. These controls do not displace the resting feed.

| Resting view, 1x scale | Original inspector | Native inspector |
| --- | --- | --- |
| Desktop, 1200 x 800 | Sidebar 224 wide; headers 46 high | 224; 46 |
| Desktop, thread selected | Thread begins at x=760, width 440; feed remains open | x=760, width 440; independent scroll regions |
| Activity message, desktop | Avatar at x=238, width 28; text starts at x=280 | x=238, width 28; text starts at x=280 |
| Split view composers | Main: x=244, y=643, 496 x 137. Reply: x=780, y=680, 400 x 100 | Main: x=244, y=642, 496 x 138. Reply: x=780, y=680, 400 x 100 |
| Compact, 390 x 844 | Hidden sidebar; full-width thread; reply editor x=20, y=724, 350 x 100 | Same layout and editor bounds |
| Empty surface, sampled from PNGs | Light `#fafafa`; dark `#040404` | Light `#fafafa`; dark `#141517` |

Compare the native [light desktop](inspector-thread.png), [dark desktop](inspector-dark.png)
and [compact thread](inspector-compact.png). Both themes use the same monochrome
hierarchy, with the text color as the accent. The feed and reply editors stay at the
bottom, not below the last off-screen message. On desktop switching between them
keeps both panes and preserves the separate drafts. At 900 points or less, closing
the thread returns to the selected channel or activity.

The comparison also included the task list and settings. Tasks use nine lifecycle
tabs, a compact table, and an inline task composer. Selecting a task puts its
ownership, claim controls, conversation and reply editor in the same pane.
The table keeps its position on desktop; narrow windows give the task the full width.
Settings keeps section tabs visible and shows the active identity's actual local
permissions. The [work-view tests](../ui/tests/inspector/work_views.rs) exercise
both sizes, create a task with a file and labels, and check scoped settings.

## Readability

These are whole frames from the shipped native renderer at 1100 x 680, with the
same empty disposable board and actor. They do not contain user screenshots.

| Dark view | Before | After |
| --- | --- | --- |
| Activity composer | [Before](inspector-readability/before/activity-dark.png) | [After](inspector-readability/after/activity-dark.png) |
| Empty Tasks | [Before](inspector-readability/before/tasks-dark.png) | [After](inspector-readability/after/tasks-dark.png) |
| Access | [Before](inspector-readability/before/access-dark.png) | [After](inspector-readability/after/access-dark.png) |

The [source editor](inspector-readability/after/sources-dark.png) is the
actual Settings > Sync form on a board with no preconfigured integrations.
Saving it updates SQLite, not the provider. The [installed-binary check](../scripts/inspect-source-settings.py)
captures the empty screen, form, saved mapping and reopened configuration.
In Settings, the minus/plus controls scale the entire interface; the percentage
resets it. A [Rune input test](../ui/tests/inspector/zoom.rs) checks that zoom
reflows the sidebar and persists without writing a board event.

The [readability tests](../ui/tests/inspector/presentation.rs) compare real
pixels of an editable `#` with the channel prefix on Activity, Tasks and Access.
They check both themes for a 3:1 dashed composer edge and 4.5:1 strokes in
placeholder and empty-state text. Empty Tasks can create a task or clear its
filters. Access uses one keyboard-operable You/Actor group and a separate
Explain command. At narrow widths that group and the fields wrap.

The window requests a standard system title bar rather than drawing its
sidebar in the title bar. macOS owns the space for its traffic lights; no
hard-coded gutter is added to the Linux or Windows client area. The captures
above were rendered on Linux. They test the native client, not the macOS
window server's placement of title-bar buttons.

## Reproduce

```sh
cargo build --locked --offline --workspace --bins
python3 scripts/inspect-visuals.py /tmp/board-inspector-shots
python3 scripts/inspect-source-settings.py /tmp/board-inspector-sources
node ui/inspect-readability.mjs /tmp/board-inspector-readability
cargo test --locked --offline -p board-ui
```

Use a **new** output directory. The [capture script](../scripts/inspect-visuals.py)
creates a disposable SQLite file, seeds it via the CLI, replays the example at its
recorded message ages, checks the measured browser bounds within two layout points,
and produces five PNGs and five Rune element trees at two
window sizes. It does not need HTTP, a display server, the original checkout or a
network connection. It does need a local graphics driver. The [scripted tests](../ui/tests/inspector.rs)
also check actual hit targets, keyboard input, pane dimensions, nonblank pixels in
each pane, the 680-point drawer, editing and external changes after the window opens.

## Boundaries

This is a native rendition, not a pixel-identical browser skin. Rune uses the
system monospace face and native font rasterization, local file dialogs, and the
actual database filename and actor. Text can wrap differently with another font,
scale or platform; dark/light screenshots were verified on Linux. The inspector
has its own local actions, including a mention picker and durable read history.
The extended task options and access forms are native controls, not copies of the
browser popovers. The native text editor inserts literal Markdown with the same
bold/italic/code shortcuts; Up in an empty editor edits your last visible message.
Contextual mention and label choices appear only while editing. Task, channel,
activity and thread drafts are independent and survive restart. [Scripted native
checks](../ui/tests/inspector/composing.rs) verify the resulting SQLite writes.
Secret-bearing and external-provider operations stay out of the UI.
See the [workflow comparison](INSPECTOR-COVERAGE.md) for the remaining differences.
