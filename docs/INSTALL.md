# Install Board

Board's release packages contain the CLI (`board`, including the stdio MCP server) and the native Rune app (`board-ui`). **No Rust, Cargo, background service or account is needed to use a package.** Installing does not change any agent settings or launch Board. Run `board init` separately, as the user who runs your agents.

**Availability:** The commands below target [Board's own GitHub Releases](https://github.com/tomharmon/board/releases). They work **after a maintainer publishes a tagged release with these assets**. This checkout adds the packaging process, not an already-published release, Homebrew listing, apt repository, AUR entry or WinGet listing. For an unreleased checkout use the source build in the [README](../README.md).

## macOS: Homebrew

For macOS 14+ on Apple Silicon or Intel, with [Homebrew](https://brew.sh/) already installed, paste this one command into Terminal:

```sh
t=$(mktemp) && curl -fLsS --proto '=https' --proto-redir '=https' https://github.com/tomharmon/board/releases/latest/download/install-macos.sh -o "$t" && /bin/sh "$t"
```

The release helper downloads and hashes the release's exact `board.rb`, puts it in a dedicated **local** tap (`tomharmon/board-release-local`), then runs `brew install --cask tomharmon/board-release-local/board` (or `brew upgrade` on a subsequent run). Brew verifies the universal ZIP's pinned SHA-256, installs `Board.app` in its applications directory and links `board` and `board-ui` into `$(brew --prefix)/bin`. Start **Board** from Applications or Spotlight. Both executables live inside `Board.app/Contents/MacOS`; ordinary upgrades do not change that path. Rerun the helper for a newer release: this local tap does not track a remote branch.

You can inspect the helper and `board.rb` on the release page before running either. To use a tap you already maintain, put the downloaded `board.rb` in its `Casks/` directory and install by its fully qualified name. The release job does not push to any tap. Modern Homebrew [installs casks by name, not from a loose Ruby path](https://docs.brew.sh/Homebrew-Package-Policies); see also its [tap guide](https://docs.brew.sh/How-to-Create-and-Maintain-a-Tap). Do not disable Gatekeeper or remove quarantine. Uninstall with `brew uninstall --cask tomharmon/board-release-local/board` and optionally `brew untap tomharmon/board-release-local`.

## macOS source install

For a local-only install on macOS 14+, use a source checkout with Rust/Cargo, Python 3, and Xcode Command Line Tools installed. Run `xcode-select --install` first if the Command Line Tools are missing. From the checkout:

```sh
python3 packaging/install_local.py
open "$HOME/Applications/Board.app"
```

Choose **Options > Keep in Dock** on Board's Dock icon. This builds only for your Mac's native architecture, installs `~/Applications/Board.app` with its icon, and links `board` and `board-ui` into `~/.local/bin`. No Homebrew, universal build, published release, Apple Developer account or background service is needed. The first Cargo build may download source dependencies; this does not download a prebuilt release. Add `~/.local/bin` to your shell's PATH for CLI commands.

The installer applies and verifies an [ad hoc signature for local use](https://developer.apple.com/documentation/xcode/embedding-a-helper-tool-in-a-sandboxed-app). This is not a notarized distribution build and does not change Gatekeeper settings or remove quarantine attributes. Keep using signed release packages when sharing Board with other people.

Rerun the install command to upgrade, then reopen Board. Use `--no-build` to install existing release binaries, or `--target-dir /path/to/target` to select another Cargo target directory (`CARGO_TARGET_DIR` is also honored). The app is staged and signed before replacing an existing Board bundle; a build or signing failure leaves the installed app unchanged. Agent settings, databases and login items are not modified.

To uninstall this source build, remove `~/Applications/Board.app` in Finder and delete the two CLI symlinks at `~/.local/bin/board` and `~/.local/bin/board-ui`. Leave your board database directories in place to keep your data.

## Linux: apt or pacman

Supported release targets: **x86-64 Ubuntu 24.04+** (also compatible Debian-family systems meeting the package's declared glibc/library dependencies) and **x86-64 Arch Linux**. The GUI needs a working Wayland or X11 session and a Vulkan driver; `ffmpeg` is optional for AVIF image previews. There is no claim of an ARM, RPM or musl build. In a terminal, as a normal user with `sudo`:

```sh
t=$(mktemp) && curl -fLsS --proto '=https' --proto-redir '=https' https://github.com/tomharmon/board/releases/latest/download/install-linux.sh -o "$t" && /bin/sh "$t"
```

The release-specific helper selects the native package for your distribution, checks its embedded SHA-256, and invokes `sudo apt install` or `sudo pacman -U` **only for the package-manager step**. There are no installation hooks. Alternatively, download the package and use `sudo apt install ./board-linux-amd64.deb` or `sudo pacman -U ./board-linux-x86_64.pkg.tar.zst`. The project distributes release attestations, not a pacman signing key. If local software policy requires additional approval/signatures, follow that policy rather than disabling verification.

Open **Board** from the applications menu or type `board ui`. Both binaries live in `/usr/bin`; the package installs `/usr/share/applications/org.localboard.Board.desktop` and the matching hicolor icon. Uninstall with `sudo apt remove board-local` or `sudo pacman -R board-local`.

## Linux source install

Install Rust/Cargo, Python 3, a C compiler, `pkg-config`, and the xkbcommon, Wayland, X11, and Vulkan development libraries. The Ubuntu CI build uses `libxkbcommon-dev`, `libxkbcommon-x11-dev`, `libwayland-dev`, `libx11-dev`, and `libvulkan-dev`. A working Vulkan driver is needed to run the desktop app; `ffmpeg` is optional for AVIF still images. From the checkout:

```sh
python3 packaging/install_local.py
```

This builds the release binaries and installs `board` and `board-ui` in `~/.local/bin`, plus **Board** in your application launcher with its icon. Open Board from the launcher and pin it to your dock; no terminal or background service is required. Installation does not open the app, modify agent settings, or start it at login. Run `board init` separately when needed.

Rerun the command to upgrade, including while Board is running (reopen it to use the new version). To install already-built binaries, use `python3 packaging/install_local.py --no-build`. `--target-dir /path/to/target` selects another Cargo target directory; `CARGO_TARGET_DIR` is also honored. The launcher uses an absolute executable path, so it does not depend on your desktop session's PATH. For CLI use, ensure `~/.local/bin` is on your shell's PATH.

The launcher and icon are installed under `${XDG_DATA_HOME:-$HOME/.local/share}` (a relative `XDG_DATA_HOME` is ignored), independently of system packages. This user-local launcher takes precedence over the native package's launcher. To remove the source install, delete only its installed files:

```sh
data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
case "$data_home" in /*) ;; *) data_home="$HOME/.local/share" ;; esac
rm -f "$HOME/.local/bin/board" "$HOME/.local/bin/board-ui" \
  "$data_home/applications/org.localboard.Board.desktop" \
  "$data_home/icons/hicolor/scalable/apps/org.localboard.Board.svg"
```

Your board databases and agent settings are left untouched.

## Windows: per-user MSI

On Windows 10/11 x64, paste this one command into **PowerShell** as your normal user (or double-click the release's `Board-Windows-x64.msi`):

```powershell
$m = "$env:TEMP\Board-$([guid]::NewGuid()).msi"; Invoke-WebRequest -UseBasicParsing -ErrorAction Stop https://github.com/tomharmon/board/releases/latest/download/Board-Windows-x64.msi -OutFile $m; if ((Get-AuthenticodeSignature $m).Status -ne 'Valid') { throw 'Invalid Board installer signature' }; $p = Start-Process msiexec -Wait -PassThru -ArgumentList "/i `"$m`" /passive /norestart"; if ($p.ExitCode -notin 0,3010) { throw "Board install failed: $($p.ExitCode)" }
```

The MSI installs `%LOCALAPPDATA%\Programs\Board\board.exe` and `board-ui.exe`, adds that directory to **your** PATH, registers a **Board** Start menu shortcut with its icon, and appears in Installed Apps. Open a **new terminal** to use `board`; a running application may retain its old PATH until restarted. Future versions use the same per-user location. Remove it in Settings > Apps > Installed apps. The installer never edits the machine PATH or runs an elevated first-time setup.

## Source build options

From the checkout, `cargo build --locked --release --workspace --bins` builds the CLI and desktop binaries without installing them. To build only the CLI without desktop libraries, use `cargo build --locked --release -p board-cli`. For an offline rebuild, first run `cargo fetch --locked` while connected, then add `--offline` to the build command.

## After installation

```sh
board --version
board init
board ui
```

`board init --dry-run` previews changes without writing settings or probing the server. A normal `board init` verifies the installed server on a disposable board and reports separately whether it configured a client. No recognized client is required for the server check. Connect other MCP clients to the installed `board` with the argument `mcp`, using its absolute path if the client launched from the desktop has a different PATH. See [MCP](MCP.md). Removing a package does not delete boards or reverse user-selected harness configuration.

## Database format

Board is experimental and opens only its current SQLite format. An older database is rejected, not migrated or erased. Keep it and its original executable if you need its contents. Start a fresh board by pointing `BOARD_DB` at a new file.

## Verify a download

Every completed release includes `SHA256SUMS`, plus GitHub build-provenance attestations for the manifest, packages, cask and helpers. Check an artifact before running it, e.g. `gh attestation verify board-linux-amd64.deb --repo tomharmon/board` (likewise for `install-macos.sh`, `install-linux.sh`, ZIP or MSI). Compare its digest with the same release's verified `SHA256SUMS`: `sha256sum FILE` (Linux), `shasum -a 256 FILE` (macOS) or `Get-FileHash FILE -Algorithm SHA256` (Windows). A checksum from the same unverified download is not independent proof of origin. See [GitHub's attestation verification guide](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations).

## Build a release (maintainers)

- Release packaging scripts live in `packaging/`. Automated release publishing CI is not enabled yet; once configured, build from `Cargo.lock` on Ubuntu 24.04, macOS and Windows Server 2025 from protected `vMAJOR.MINOR.PATCH` tags matching **both** package versions.
- Set up a protected GitHub `release` environment with required reviewers, restrict it to the release tags and protect those tags. Keep `packaging-check` unprivileged. Public release assets and [GitHub artifact attestations](https://docs.github.com/en/actions/concepts/security/artifact-attestations) must be available for the repository/account. A private repo needs an authenticated internal distribution path instead of these anonymous URLs.
- **Apple:** provide environment secrets `APPLE_CERT_P12_BASE64`, `APPLE_CERT_PASSWORD` (one trusted **Developer ID Application** identity with its private key), `APPLE_NOTARY_KEY_BASE64` (App Store Connect **team** API key `.p8`), `APPLE_NOTARY_KEY_ID`, `APPLE_NOTARY_ISSUER_ID`. Use a signing certificate with its intermediate chain. The workflow signs nested CLI and app with hardened runtime and secure timestamps, waits for [Apple notarization](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow), staples, re-archives, verifies the delivered bundle with Gatekeeper, and exercises the rendered cask using the exact ZIP in Brew's cache. An unsigned or unaccepted release cannot publish. Preflight's ad hoc app is never uploaded.
- **Windows:** provision a public-trust [Microsoft Artifact Signing](https://github.com/Azure/artifact-signing-action) certificate profile and an Azure federated credential restricted to this repo's **release** environment. Grant its principal only the Certificate Profile Signer role on the profile. Set environment variables `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_SUBSCRIPTION_ID`, `AZURE_SIGNING_ENDPOINT`, `AZURE_SIGNING_ACCOUNT`, `AZURE_SIGNING_PROFILE`. No long-lived Azure signing key is checked in. The native WiX build embeds the icon in the UI and MSI; tagged releases must pass timestamped Authenticode verification for the MSI **and both installed binaries**. The Windows test installs and uninstalls on the disposable runner and checks the installed PATH, shortcut, icon, CLI, Rune binary and restored PATH. Signing does not guarantee SmartScreen reputation.
- **Linux:** the builder stages only the two binaries, desktop entry, icon and notices. `dpkg-deb --root-owner-group` and `makepkg` make the archives; dependency metadata includes the actual maximum referenced glibc symbol version. CI extracts each real package, validates the desktop entry, runs the installed CLI and renders a Rune frame with Mesa in an isolated HOME. Locally, with `dpkg-deb`, `makepkg`, `fakeroot`, `bsdtar`, `desktop-file-validate` and a Vulkan ICD available:

```sh
cargo build --locked --release -p board-cli -p board-ui --bins
python3 packaging/linux.py build "v$(python3 packaging/release.py version)"
python3 packaging/linux.py verify "v$(python3 packaging/release.py version)"
python3 -m unittest discover -s packaging -p 'test_*.py'
```

Local packages appear in `target/packages/`. `VK_DRIVER_FILES` can point at a software Vulkan ICD on displayless Linux builders. All three native jobs exercise `board init --dry-run` from the installed path in an empty user profile. Before distributing, launch the app by name from Spotlight, the Linux applications menu or Start on a clean target machine, confirm the window opens, run `board init` as that user, then check a fresh harness session. These Linux checks do **not** substitute for native release signing or macOS/Windows CI. The source icon is `packaging/icons/board.svg`; refresh its derived inputs with `rsvg-convert packaging/icons/board.svg -o packaging/icons/board.png` and `magick packaging/icons/board.png -define icon:auto-resize=256,64,48,32,24,16 packaging/icons/board.ico`.
