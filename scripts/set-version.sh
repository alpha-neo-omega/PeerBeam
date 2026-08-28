#!/usr/bin/env bash
# Single-source the version across Rust, Flutter and packaging.
# Usage: set-version.sh [X.Y.Z]
# With no arg, syncs everything to the contents of ./VERSION.
set -euo pipefail
cd "$(dirname "$0")/.."
VER="${1:-$(cat VERSION)}"
echo "$VER" > VERSION

# Rust workspace version (rust/Cargo.toml [workspace.package]).
sed -i -E "s/^version = \"[0-9]+\.[0-9]+\.[0-9]+\"/version = \"$VER\"/" rust/Cargo.toml

# Flutter version, and the Android build number **incremented** with it.
#
# The `+N` suffix is Android's `versionCode`, and Android refuses to install a
# package whose code is not greater than the installed one. It does not fail
# loudly at build time — it fails at *install* time, on a user's phone, as
# "app not installed". So a release that reuses the previous number is one
# nobody can upgrade to, and nothing in CI would have said so.
#
# This used to only preserve the existing number, and it was bumped by hand
# every release (14 → 15 → 16 for v0.8.2 → v0.9.0 → v0.10.0). That worked
# until somebody forgot. Bumping only when the marketing version actually
# changes keeps a re-run of `set-version.sh` on the same version idempotent.
current="$(grep -m1 '^version:' flutter/pubspec.yaml)"
build="$(printf '%s' "$current" | sed -E 's/.*\+([0-9]+).*/\1/')"
[ "$build" = "$current" ] && build=0
prev_ver="$(printf '%s' "$current" | sed -E 's/^version: ([0-9.]+).*/\1/')"
if [ "$prev_ver" != "$VER" ]; then
  build=$((build + 1))
fi
sed -i -E "s/^version: .*/version: $VER+$build/" flutter/pubspec.yaml

# Arch package version (packaging/arch/PKGBUILD).
#
# `pkgver` is not just a label there: the `source=()` line builds both the
# release tag it downloads (`.../download/v$pkgver/`) and the tarball name out
# of it. A stale `pkgver` therefore does not fail — it quietly fetches and
# installs an older release on a newer checkout, and `makepkg -si` reports
# success. Nothing here used to touch it, so it sat at 0.8.0 through v0.9.0
# and into v0.10.0 before anyone noticed.
sed -i -E "s/^pkgver=[0-9]+\.[0-9]+\.[0-9]+/pkgver=$VER/" packaging/arch/PKGBUILD

# Scaffold this version's release notes if they do not exist yet.
#
# `.github/workflows/release.yml` publishes with `docs/RELEASE_NOTES_v<VER>.md`
# when it is present. It used to *require* the file and failed the release job
# without it — after every platform had already built — which is how v0.4.1,
# v0.5.0 and v0.6.0 ended up tagged with no release at all. The workflow now
# falls back to generated notes, so a missing file costs quality rather than the
# release; this scaffold is the other half, catching it at the one moment
# somebody is already thinking about the version.
notes="docs/RELEASE_NOTES_v${VER}.md"
if [ -e "$notes" ]; then
  echo "release notes: $notes (exists)"
else
  cat > "$notes" <<NOTES
# PeerBeam v${VER} — Beta

<!-- One or two sentences: what this release is for. Written for someone
     deciding whether to upgrade, not a commit log. -->

## Highlights

<!-- What changed that a user would notice, and why it matters. -->

## Upgrade note

<!-- Anything that behaves differently after upgrading. Delete if nothing does. -->

## Downloads

Linux (\`.deb\`, \`.tar.gz\`), Windows (portable \`.zip\`), Android
(\`.apk\`/\`.aab\`), macOS (universal \`.dmg\`) and the standalone CLI are
attached below. Desktop and CLI artifacts are **unsigned**.

Full detail in [CHANGELOG.md](../CHANGELOG.md).
NOTES
  echo "release notes: $notes (created — fill it in before tagging)"
fi

echo "version set to $VER (flutter build $build)"
grep -m1 '^version' rust/Cargo.toml
grep -m1 '^version:' flutter/pubspec.yaml
grep -m1 '^pkgver=' packaging/arch/PKGBUILD
