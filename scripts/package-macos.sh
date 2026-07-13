#!/usr/bin/env bash
# Package the macOS app as a signed, notarization-ready DMG. Run on macOS.
# Env (optional): PB_SIGN_ID (Developer ID Application), PB_TEAM_ID,
#   PB_NOTARY_PROFILE (a stored notarytool keychain profile).
set -euo pipefail
cd "$(dirname "$0")/.."
VER="$(cat VERSION)"

echo "== build engine + flutter (release) =="
cargo build --manifest-path rust/Cargo.toml --release -p peerbeam-ffi
( cd flutter && flutter build macos --release )
APP="flutter/build/macos/Build/Products/Release/peerbeam.app"
[ -d "$APP" ] || { echo "app not found: $APP"; exit 1; }

if [ -n "${PB_SIGN_ID:-}" ]; then
  echo "== codesign (hardened runtime) =="
  codesign --deep --force --options runtime \
    --entitlements flutter/macos/Runner/Release.entitlements \
    --sign "$PB_SIGN_ID" "$APP"
else
  echo "skip codesign (PB_SIGN_ID unset) — DMG will be unsigned"
fi

echo "== DMG =="
mkdir -p dist
DMG="dist/PeerBeam-${VER}.dmg"
if command -v create-dmg >/dev/null; then
  create-dmg --volname "PeerBeam" --app-drop-link 400 200 "$DMG" "$APP" || true
else
  # Fallback: plain DMG from a staging dir.
  STAGE="$(mktemp -d)"; cp -R "$APP" "$STAGE/"; ln -s /Applications "$STAGE/Applications"
  hdiutil create -volname PeerBeam -srcfolder "$STAGE" -ov -format UDZO "$DMG"
fi
echo "OK  $DMG"

if [ -n "${PB_NOTARY_PROFILE:-}" ]; then
  echo "== notarize + staple =="
  xcrun notarytool submit "$DMG" --keychain-profile "$PB_NOTARY_PROFILE" --wait
  xcrun stapler staple "$DMG"
else
  echo "skip notarize (PB_NOTARY_PROFILE unset)"
fi
echo "== done =="
