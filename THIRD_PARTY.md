# Third-party materials

Board's Rust source is licensed under [Apache-2.0](LICENSE). The reference
board informed the behavior; this repository does not ship its implementation,
templates, graphics or documentation. The screenshots are rendered by this
application from a disposable local database.

- [Anomaly Rune](https://github.com/anomalyco/rune/tree/44920b1fdfe6d1b7156d80b90f0ca0d96080d2bf)
  provides the native framework and rune-kit. Its [MIT notice](ui/licenses/Rune-MIT.txt)
  for the framework, the [SIL Open Font License and Inter notice](ui/licenses/Inter-OFL.txt)
  for the embedded faces, and the [Apache license for the code highlighter](ui/licenses/Highlights-Apache.txt)
  are included. All three are available offline from the
  executable with `board-ui --ui-notices`. The manifest patches select Rune's
  pinned upstream forks rather than copying them into Board.
- SQLite, rusqlite, Axum, Tokio, Clap, and other dependencies come from the
  sources and versions in [Cargo.lock](Cargo.lock). Their original licenses
  apply independently. The source checkout does not vendor dependency code.
- A small set of glyphs from [Lucide](https://lucide.dev/) 1.49.0 is compiled
  into the native UI. Its [ISC and Feather MIT notices](ui/licenses/Lucide.txt)
  are included in both the source and `board-ui --ui-notices`.
- [Rhai](https://github.com/rhaiscript/rhai) is the embedded, operation-limited
  rules interpreter (MIT or Apache-2.0). [FFmpeg](https://ffmpeg.org/legal.html)
  is optional for silent local H.264 previews. Board does not bundle FFmpeg
  or a codec library; the user's installed executable retains its own license.

For packaging, use `cargo metadata --offline --locked --format-version 1`
after fetching the locked dependencies. Its package entries identify source
paths, license expressions and declared license files. Include the applicable
dependency copyright, license and notice files when redistributing binaries;
Board's own LICENSE does not replace them.
