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
# Four ecosystems, four spellings of the same architecture. Derived once from
# the host rather than hardcoded, so this script packages an arm64 build without
# renaming anything: Flutter says x64/arm64, dpkg says amd64/arm64, rpm and
# AppImage say x86_64/aarch64, and our own filenames follow Flutter.
case "$(uname -m)" in
  x86_64|amd64)  FARCH=x64;   DARCH=amd64; RARCH=x86_64  ;;
  aarch64|arm64) FARCH=arm64; DARCH=arm64; RARCH=aarch64 ;;
  *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac
BUNDLE="flutter/build/linux/$FARCH/release/bundle"
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


# ---- tray libraries, for the formats that cannot declare a dependency ----
#
# `libtray_manager_plugin.so` is a direct NEEDED of the runner, so the dynamic
# linker resolves its own NEEDED chain — Ayatana's app-indicator and dbusmenu —
# at process start. A host without them does not lose the tray icon: the app
# does not launch at all.
#
# `.deb`, `.rpm` and the PKGBUILD declare that dependency and correctly use the
# system copy. The **tarball and AppImage cannot declare anything**, and "any
# distribution, installs nothing" is exactly what the AppImage promises, so for
# those two the chain travels with the payload — together with a launcher that
# sets `LD_LIBRARY_PATH`.
#
# The launcher is not optional. The runner's own RUNPATH is `$ORIGIN/lib`, but
# the library that needs the chain is `libtray_manager_plugin.so`, whose RUNPATH
# is an absolute path into the BUILD machine's Flutter ephemeral directory — and
# RUNPATH, unlike the older RPATH, is not inherited by a dependency's own
# dependencies. So a copy dropped in `lib/` is invisible to the plugin that
# needs it, and only an explicit search path makes it findable.
#
# Deliberately narrow: only the indicator/dbusmenu chain, resolved from the
# plugin itself. Bundling everything `ldd` reports would drag GTK and glib along
# and produce the cross-distro breakage that bundling is supposed to avoid.
bundle_tray_libs() {
  local dest="$1/lib" plugin="$1/lib/libtray_manager_plugin.so"
  [ -f "$plugin" ] || { echo "FAIL: $plugin missing" >&2; exit 1; }
  local found=0
  while read -r name _arrow path _rest; do
    case "$name" in
      libayatana-*|libdbusmenu-*|libindicator*)
        [ -f "$path" ] || continue
        cp -L "$path" "$dest/$name"
        found=$((found + 1))
        ;;
    esac
  done < <(ldd "$plugin")
  if [ "$found" -eq 0 ]; then
    echo "FAIL: no app-indicator libraries found to bundle. Install " \
         "libayatana-appindicator3-dev (Debian) or " \
         "libayatana-appindicator-gtk3-devel (Fedora) and rebuild — without " \
         "them this AppImage/tarball would not start on any host that also " \
         "lacks them, which is every host an AppImage exists for." >&2
    exit 1
  fi
  echo "    bundled $found tray libraries into $(basename "$1")/lib"
}

# ---- tar.gz (always) ----
#
# Built from a COPY of the staging tree with the tray libraries added. `$STAGE`
# itself stays clean, because `.deb` and `.rpm` are built from it and they
# declare the dependency instead — shipping a second copy inside those packages
# would shadow the system's.
PORTABLE="$DIST/portable"
rm -rf "$PORTABLE"; cp -r "$STAGE" "$PORTABLE"
bundle_tray_libs "$PORTABLE/opt/$APP"
# A launcher rather than the plain symlink `$STAGE` carries, so the bundled
# chain is actually found — see `bundle_tray_libs` for why the rpath cannot do
# it. `exec` so the process is replaced and signals reach the app unchanged.
rm -f "$PORTABLE/usr/bin/$APP"
cat > "$PORTABLE/usr/bin/$APP" <<LAUNCH
#!/bin/sh
# PeerBeam portable launcher. The app's own libraries ship beside it.
exec env LD_LIBRARY_PATH="/opt/$APP/lib\${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}" \
  "/opt/$APP/$APP" "\$@"
LAUNCH
chmod 755 "$PORTABLE/usr/bin/$APP"
TGZ="$DIST/${APP}-${VER}-linux-${FARCH}.tar.gz"
tar -C "$PORTABLE" -czf "$TGZ" .
rm -rf "$PORTABLE"
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
Architecture: $DARCH
Depends: libgtk-3-0, libayatana-appindicator3-1
Maintainer: PeerBeam Contributors <noreply@peerbeam>
Description: Secure, zero-config file & clipboard sharing
CTRL
  dpkg-deb --build --root-owner-group "$DEB" "$DIST/${APP}-${VER}-${DARCH}.deb"
  echo "OK  $DIST/${APP}-${VER}-${DARCH}.deb"
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
BuildArch:      $RARCH
# The GUI links GTK3 and Ayatana's app-indicator (the tray icon) at runtime;
# everything else is static in the bundle. Both are hard requirements rather
# than optional: they are NEEDED entries on the binary, so a missing one means
# the app does not start at all rather than losing a feature.
Requires:       gtk3
Requires:       libayatana-appindicator-gtk3
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
    mv "$RPMOUT" "$DIST/${APP}-${VER}-${RARCH}.rpm"
    rm -rf "$RPMTOP"
    echo "OK  $DIST/${APP}-${VER}-${RARCH}.rpm"
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
  bundle_tray_libs "$APPDIR"
  cp packaging/linux/peerbeam.desktop "$APPDIR/$APP.desktop"
  [ -f "$ICONS/256.png" ] && cp "$ICONS/256.png" "$APPDIR/$APP.png"
  # A wrapper, not a symlink to the binary: the bundled indicator chain is only
  # findable through an explicit search path (see `bundle_tray_libs`). Without
  # this the AppImage does not start on a host that lacks those libraries, which
  # is the only kind of host an AppImage exists for.
  cat > "$APPDIR/AppRun" <<'LAUNCH'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
exec env LD_LIBRARY_PATH="$HERE/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
  "$HERE/peerbeam" "$@"
LAUNCH
  chmod 755 "$APPDIR/AppRun"
  # appimagetool cannot always infer the architecture from the payload; state it.
  ARCH=$RARCH appimagetool "$APPDIR" "$DIST/${APP}-${VER}-${RARCH}.AppImage"
  rm -rf "$APPDIR"
  echo "OK  $DIST/${APP}-${VER}-${RARCH}.AppImage"
else
  echo "skip AppImage (appimagetool absent)"
fi

echo "== done. artifacts in $DIST/ =="
