# Inspector visual comparison

The target was the local inspector in reference revision
`c2d3324b117adaa8e9a0107e15ea2fca207c95ac`, not a screenshot inferred from its README.
We built its production inspector in a disposable directory, started its local backend
and inspected the rendered page in Chromium. The comparison board had three channels,
four root posts, one reply, a label and a reaction. We rendered the same conversations
through the Rust CLI and the actual Rune binary. The screenshots in this repository
contain only the native application; no browser bundle, web fonts or reference
screenshots are shipped.

| Resting view, 1x scale | Original inspector | Native inspector |
| --- | --- | --- |
| Desktop, 1200 x 800 | Sidebar 224 wide; headers 46 high | 224; 46 |
| Desktop, thread selected | Thread begins at x=760, width 440; feed remains open | x=760, width 440; independent scroll regions |
| Activity message, desktop | Avatar at x=238, width 28; text starts at x=280 | x=238, width 28; text starts at x=280 |
| Split view composers | Main: x=244, y=643, 496 x 137. Reply: x=780, y=680, 400 x 100 | Main: x=244, y=644, 496 x 136. Reply: x=780, y=680, 400 x 100 |
| Compact, 390 x 844 | Hidden sidebar; full-width thread; reply editor x=20, y=724, 350 x 100 | Same layout and editor bounds |
| Empty surface, sampled from PNGs | Light `#fafafa`; dark `#040404` | Same RGB values in both captures |

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

## Reproduce

```sh
cargo build --locked --offline --workspace --bins
python3 scripts/inspect-visuals.py /tmp/board-inspector-shots
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
browser popovers. The composer accepts Markdown as text; browser formatting
shortcuts and editing the last post with Up in an empty editor are not reproduced.
Task-entry drafts stay in the running window; message and reply drafts survive restart.
Secret-bearing and external-provider operations stay out of the UI.
See the [workflow comparison](INSPECTOR-COVERAGE.md) for the remaining differences.
