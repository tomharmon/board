#!/bin/bash
# Run on a disposable macOS runner, never against a user's installed app or tap.
set -euo pipefail
tag=${1:?usage: verify.sh vX.Y.Z release|preview}
mode=${2:?}
case "$mode" in release|preview) ;; *) exit 2 ;; esac
test "${CI:-}" = true || { echo 'Run the Homebrew installation test on a disposable CI runner.' >&2; exit 1; }
export HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_ANALYTICS=1 HOMEBREW_NO_ASK=1
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export BOARD_DB="$tmp/board.sqlite3"
out=target/packages
mkdir -p "$out" "$tmp/unzip" "$tmp/Applications"
zip="$out/Board-macOS-universal.zip"
ditto -c -k --keepParent --sequesterRsrc target/package-mac/Board.app "$zip"
ditto -x -k "$zip" "$tmp/unzip"
app="$tmp/unzip/Board.app"
test "$(/usr/libexec/PlistBuddy -c 'Print CFBundleExecutable' "$app/Contents/Info.plist")" = 'board-ui'
test -s "$app/Contents/Resources/Board.icns"
codesign --verify --deep --strict "$app"
for bin in board board-ui; do lipo -verify_arch x86_64 arm64 "$app/Contents/MacOS/$bin"; done
if [[ "$mode" == release ]]; then
    xcrun stapler validate "$app"
    spctl --assess --type execute --verbose=4 "$app"
fi
"$app/Contents/MacOS/board" --version | grep -F "${tag#v}"
"$app/Contents/MacOS/board-ui" --ui-notices > "$tmp/notices"
grep -q Rune "$tmp/notices"
python3 packaging/release.py mac-manifest "$tag" "$out"
tap=tomharmon/board-package-check
brew tap-new --no-git "$tap"
dir=$(brew --repository "$tap")
mkdir -p "$dir/Casks"
cp "$out/board.rb" "$dir/Casks/board.rb"
if brew command trust >/dev/null 2>&1; then brew trust --cask "$tap/board"; fi
brew info --json=v2 --cask "$tap/board" > "$tmp/cask.json"
# The release does not exist yet. Seed Brew's download cache with these exact bytes.
cache=$(brew --cache --cask "$tap/board")
mkdir -p "$(dirname "$cache")"
cp "$zip" "$cache"
brew install --cask --appdir="$tmp/Applications" "$tap/board"
test -L "$(brew --prefix)/bin/board"
test -L "$(brew --prefix)/bin/board-ui"
"$(brew --prefix)/bin/board" --version | grep -F "${tag#v}"
python3 packaging/smoke_init.py "$(brew --prefix)/bin/board"
test -x "$tmp/Applications/Board.app/Contents/MacOS/board-ui"
brew uninstall --cask "$tap/board"
brew untap "$tap"
