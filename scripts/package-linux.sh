#!/usr/bin/env bash
# Package the Linux desktop app. Always produces a portable tar.gz; also builds
# .deb / .rpm / AppImage when the respective tools are present (skipped, not
# failed, when absent).
set -euo pipefail
cd "$(dirname "$0")/.."
VER="$(cat VERSION)"
DIST="dist"
APP="peerbeam"
mkdir -p "$DIST"

echo "== build engine + flutter (release) =="
bash scripts/build-ffi.sh release
( cd flutter && flutter build linux --release )
BUNDLE="flutter/build/linux/x64/release/bundle"
[ -d "$BUNDLE" ] || { echo "flutter bundle missing: $BUNDLE"; exit 1; }

# Render hicolor icon sizes from the brand master (packaging/icon-1024.png).
ICONS="$DIST/icons"
mkdir -p "$ICONS"
MASTER="packaging/icon-1024.png"
if command -v magick >/dev/null; then
  for s in 32 64 128 256 512; do
    magick "$MASTER" -resize ${s}x${s} "$ICONS/${s}.png"
  done
elif command -v convert >/dev/null; then
  for s in 32 64 128 256 512; do
    convert "$MASTER" -resize ${s}x${s} "$ICONS/${s}.png"
  done
else
  echo "WARN: no rasterizer; icons will be missing"
fi

# ---- staging tree (FHS layout) ----
STAGE="$DIST/stage"
rm -rf "$STAGE"
install -d "$STAGE/opt/$APP" "$STAGE/usr/bin" \
  "$STAGE/usr/share/applications" "$STAGE/usr/share/metainfo"
cp -r "$BUNDLE"/. "$STAGE/opt/$APP/"
ln -sf "/opt/$APP/$APP" "$STAGE/usr/bin/$APP"
cp packaging/linux/peerbeam.desktop "$STAGE/usr/share/applications/$APP.desktop"
for s in 32 64 128 256 512; do
  if [ -f "$ICONS/${s}.png" ]; then
    install -Dm644 "$ICONS/${s}.png" \
      "$STAGE/usr/share/icons/hicolor/${s}x${s}/apps/$APP.png"
  fi
done

# ---- tar.gz (always) ----
TGZ="$DIST/${APP}-${VER}-linux-x64.tar.gz"
tar -C "$STAGE" -czf "$TGZ" .
echo "OK  $TGZ"

# ---- .deb (if dpkg-deb) ----
if command -v dpkg-deb >/dev/null; then
  DEB="$DIST/deb"; rm -rf "$DEB"; cp -r "$STAGE" "$DEB"
  install -d "$DEB/DEBIAN"
  cat > "$DEB/DEBIAN/control" <<CTRL
Package: $APP
Version: $VER
Section: net
Priority: optional
Architecture: amd64
Maintainer: PeerBeam Contributors <noreply@peerbeam>
Description: Secure, zero-config file & clipboard sharing
CTRL
  dpkg-deb --build --root-owner-group "$DEB" "$DIST/${APP}-${VER}-amd64.deb"
  echo "OK  $DIST/${APP}-${VER}-amd64.deb"
else
  echo "skip .deb (dpkg-deb absent)"
fi

# ---- .rpm (if rpmbuild) ----
if command -v rpmbuild >/dev/null; then
  echo "rpmbuild present — see docs/BUILD.md for the .spec flow"
else
  echo "skip .rpm (rpmbuild absent)"
fi

# ---- AppImage (if appimagetool) ----
if command -v appimagetool >/dev/null; then
  APPDIR="$DIST/${APP}.AppDir"; rm -rf "$APPDIR"; install -d "$APPDIR"
  cp -r "$BUNDLE"/. "$APPDIR/"
  cp packaging/linux/peerbeam.desktop "$APPDIR/$APP.desktop"
  [ -f "$ICONS/256.png" ] && cp "$ICONS/256.png" "$APPDIR/$APP.png"
  ln -sf "$APP" "$APPDIR/AppRun"
  appimagetool "$APPDIR" "$DIST/${APP}-${VER}-x86_64.AppImage"
  echo "OK  AppImage"
else
  echo "skip AppImage (appimagetool absent)"
fi

echo "== done. artifacts in $DIST/ =="
