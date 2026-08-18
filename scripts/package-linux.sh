#!/usr/bin/env bash
# Package the Linux desktop app. Always produces a portable tar.gz; also builds
# .deb / .rpm / AppImage when the respective tools are present (skipped, not
# failed, when absent).
set -euo pipefail
cd "$(dirname "$0")/.."
# In CI the tag is the source of truth; VERSION is the local fallback.
VER="${GITHUB_REF_NAME:-}"
VER="${VER#v}"
# ...but only when the ref actually is a version tag. The workflow also offers
# `workflow_dispatch`, where GITHUB_REF_NAME is the *branch* — which produced
# `peerbeam-main-linux-x64.tar.gz` and a .deb whose Version field was literally
# `main`, so dpkg-deb refused it ("version number does not start with digit")
# and every manual run of the release workflow died there. A ref that is not a
# version falls back to VERSION, the same source `set-version.sh` writes.
case "$VER" in
  [0-9]*) ;;
  *) VER="" ;;
esac
[ -n "$VER" ] || VER="$(cat VERSION)"
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
#
# This used to print "see docs/BUILD.md for the .spec flow" and build nothing,
# while the header above claimed .rpm was produced when the tool was present.
# CI installs `rpm`, so it took this branch every release and shipped no package
# at all — Fedora, RHEL and openSUSE had only the tarball.
#
# Built from the same $STAGE tree as the .deb, so the two cannot describe
# different layouts. rpmbuild insists on its own directory tree, hence the
# --define overrides rather than touching ~/rpmbuild.
if command -v rpmbuild >/dev/null; then
  RPMTOP="$DIST/rpmbuild"; rm -rf "$RPMTOP"
  install -d "$RPMTOP"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}
  # rpm rejects a dash in Version; keep the tag's exact string in Release-free
  # form and let it fail loudly rather than silently mangling a pre-release.
  RPMVER="${VER%%-*}"
  cat > "$RPMTOP/SPECS/$APP.spec" <<SPEC
Name:           $APP
Version:        $RPMVER
Release:        1%{?dist}
Summary:        Secure, zero-config file & clipboard sharing
License:        AGPL-3.0-or-later
URL:            https://github.com/alpha-neo-omega/PeerBeam
BuildArch:      x86_64
# The GUI links GTK3 at runtime; everything else is static in the bundle.
Requires:       gtk3
# The payload is a prebuilt Flutter bundle: already stripped, and its .so files
# are not meant to be picked apart by rpm's automatic dependency generator.
AutoReqProv:    no
%global __os_install_post %{nil}

%description
PeerBeam discovers peers across LAN, mDNS and Tailscale at once and streams
files of any size with end-to-end encryption, resumable integrity-checked
transfers, chat, clipboard sync and presence. No accounts, no cloud.

%install
cp -a %{_sourcedir}/stage/. %{buildroot}/

%files
/opt/$APP
/usr/bin/$APP
/usr/share/applications/$APP.desktop
$(cd "$STAGE" && find usr/share/icons -name "$APP.png" 2>/dev/null | sed 's|^|/|')

%changelog
* $(LC_ALL=C date '+%a %b %d %Y') PeerBeam Contributors <noreply@peerbeam> - $RPMVER-1
- Release $VER
SPEC
  install -d "$RPMTOP/SOURCES/stage"
  cp -a "$STAGE"/. "$RPMTOP/SOURCES/stage/"
  rpmbuild -bb "$RPMTOP/SPECS/$APP.spec" \
    --define "_topdir $(cd "$RPMTOP" && pwd)" \
    --define "_sourcedir $(cd "$RPMTOP" && pwd)/SOURCES" \
    --define "_buildrootdir $(cd "$RPMTOP" && pwd)/BUILDROOT" >/dev/null
  RPMOUT=$(find "$RPMTOP/RPMS" -name '*.rpm' -type f | head -1)
  if [ -n "$RPMOUT" ]; then
    mv "$RPMOUT" "$DIST/${APP}-${VER}-x86_64.rpm"
    rm -rf "$RPMTOP"
    echo "OK  $DIST/${APP}-${VER}-x86_64.rpm"
  else
    echo "FAIL .rpm: rpmbuild produced nothing" >&2
    exit 1
  fi
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
  # appimagetool cannot always infer the architecture from the payload; state it.
  ARCH=x86_64 appimagetool "$APPDIR" "$DIST/${APP}-${VER}-x86_64.AppImage"
  rm -rf "$APPDIR"
  echo "OK  $DIST/${APP}-${VER}-x86_64.AppImage"
else
  echo "skip AppImage (appimagetool absent)"
fi

echo "== done. artifacts in $DIST/ =="
