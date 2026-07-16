#!/usr/bin/env bash
# Package the macOS app as a signed, notarization-ready DMG. Run on macOS.
# Env (optional): PB_SIGN_ID (Developer ID Application), PB_TEAM_ID,
#   PB_NOTARY_PROFILE (a stored notarytool keychain profile).
set -euo pipefail
cd "$(dirname "$0")/.."
VER="$(cat VERSION)"

echo "== build engine (universal: x86_64 + arm64) =="
# The Flutter macOS app is a universal binary; the embedded engine must be too,
# or it crashes on the arch the dylib lacks (e.g. an Intel Mac loading an
# arm64-only dylib).
rustup target add x86_64-apple-darwin aarch64-apple-darwin
cargo build --manifest-path rust/Cargo.toml --release -p peerbeam-ffi \
  --target x86_64-apple-darwin --target aarch64-apple-darwin

echo "== build flutter (release) =="
( cd flutter && flutter build macos --release )
APP="flutter/build/macos/Build/Products/Release/peerbeam.app"
[ -d "$APP" ] || { echo "app not found: $APP"; exit 1; }

echo "== embed engine dylib into the app bundle =="
# macOS dlopen(leaf-name) does not search the bundle, so the app loads the
# engine by an explicit ../Frameworks path (see flutter/lib/sdk/ffi/bindings.dart);
# place a universal dylib there with an @rpath install id.
FW="$APP/Contents/Frameworks"
mkdir -p "$FW"
DYLIB="$FW/libpeerbeam_ffi.dylib"
lipo -create \
  "rust/target/x86_64-apple-darwin/release/libpeerbeam_ffi.dylib" \
  "rust/target/aarch64-apple-darwin/release/libpeerbeam_ffi.dylib" \
  -output "$DYLIB"
install_name_tool -id @rpath/libpeerbeam_ffi.dylib "$DYLIB"
echo "   embedded $(lipo -archs "$DYLIB") -> $DYLIB"

if [ -n "${PB_SIGN_ID:-}" ]; then
  echo "== codesign (Developer ID, hardened runtime) =="
  # Sign the nested dylib first, then seal the app. Same signing identity, so
  # library validation under the hardened runtime accepts the embedded engine.
  codesign --force --options runtime --sign "$PB_SIGN_ID" "$DYLIB"
  codesign --force --deep --options runtime \
    --entitlements flutter/macos/Runner/Release.entitlements \
    --sign "$PB_SIGN_ID" "$APP"
else
  # No cert: ad-hoc sign so the embedded dylib is sealed and the app launches
  # after the user clears quarantine (xattr -dr com.apple.quarantine). No
  # hardened runtime here, so library validation does not block the ad-hoc dylib.
  echo "== ad-hoc codesign (PB_SIGN_ID unset) — DMG unsigned/un-notarized =="
  codesign --force --sign - "$DYLIB"
  codesign --force --deep --sign - "$APP"
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
