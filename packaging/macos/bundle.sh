#!/bin/bash
set -euo pipefail
tag=${1:?usage: bundle.sh vX.Y.Z}
version=$(python3 packaging/release.py version "$tag")
app=target/package-mac/Board.app
test ! -e "$app" || { echo "Use a clean staging directory: $app" >&2; exit 1; }
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
sed "s/@VERSION@/$version/g" packaging/macos/Info.plist.in > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
for bin in board-ui board; do
    lipo -create "target/aarch64-apple-darwin/release/$bin" "target/x86_64-apple-darwin/release/$bin" -output "$app/Contents/MacOS/$bin"
    lipo -verify_arch arm64 x86_64 "$app/Contents/MacOS/$bin"
    chmod 755 "$app/Contents/MacOS/$bin"
done
icons=target/package-mac/Board.iconset
mkdir -p "$icons"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" packaging/icons/board.png --out "$icons/icon_${size}x${size}.png" >/dev/null
    sips -z "$((size*2))" "$((size*2))" packaging/icons/board.png --out "$icons/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$icons" -o "$app/Contents/Resources/Board.icns"
python3 packaging/release.py notices aarch64-apple-darwin "$app/Contents/Resources/NOTICES-arm64.txt"
python3 packaging/release.py notices x86_64-apple-darwin "$app/Contents/Resources/NOTICES-x64.txt"
