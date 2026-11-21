#!/bin/bash
# Trusted release step only. No cargo/build scripts run with these environment variables.
set -euo pipefail
umask 077
: "${APPLE_CERT_P12_BASE64:?Developer ID Application PKCS12 required}"
: "${APPLE_CERT_PASSWORD:?PKCS12 password required}"
: "${APPLE_NOTARY_KEY_BASE64:?App Store Connect team API key required}"
: "${APPLE_NOTARY_KEY_ID:?}" "${APPLE_NOTARY_ISSUER_ID:?}"
app=target/package-mac/Board.app
tmp=$(mktemp -d)
keychain="$tmp/build.keychain-db"
cleanup() { security delete-keychain "$keychain" >/dev/null 2>&1 || true; rm -rf "$tmp"; }
trap cleanup EXIT
password=$(openssl rand -hex 32)
printf '%s' "$APPLE_CERT_P12_BASE64" | base64 -D > "$tmp/cert.p12"
printf '%s' "$APPLE_NOTARY_KEY_BASE64" | base64 -D > "$tmp/key.p8"
security create-keychain -p "$password" "$keychain"
security set-keychain-settings -lut 7200 "$keychain"
security unlock-keychain -p "$password" "$keychain"
security import "$tmp/cert.p12" -k "$keychain" -P "$APPLE_CERT_PASSWORD" -T /usr/bin/codesign >/dev/null
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" "$keychain" >/dev/null
identity=$(security find-identity -v -p codesigning "$keychain" | awk '/"Developer ID Application: / {print $2}')
[[ "$identity" =~ ^[0-9A-Fa-f]{40}$ ]] || { echo 'Expected exactly one valid Developer ID Application identity.' >&2; exit 1; }
umask 022
codesign --force --sign "$identity" --keychain "$keychain" --options runtime --timestamp --identifier org.localboard.Board.cli "$app/Contents/MacOS/board"
codesign --force --sign "$identity" --keychain "$keychain" --options runtime --timestamp "$app"
codesign --verify --deep --strict --verbose=2 "$app"
ditto -c -k --keepParent --sequesterRsrc "$app" "$tmp/submission.zip"
xcrun notarytool submit "$tmp/submission.zip" --key "$tmp/key.p8" --key-id "$APPLE_NOTARY_KEY_ID" --issuer "$APPLE_NOTARY_ISSUER_ID" --wait --timeout 45m --output-format json > "$tmp/result.json"
python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); print("Notary:", r["id"], r["status"]); sys.exit(r["status"] != "Accepted")' "$tmp/result.json"
xcrun stapler staple "$app"
xcrun stapler validate "$app"
spctl --assess --type execute --verbose=4 "$app"
