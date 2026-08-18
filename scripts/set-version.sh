#!/usr/bin/env bash
# Single-source the version across Rust + Flutter. Usage: set-version.sh [X.Y.Z]
# With no arg, syncs everything to the contents of ./VERSION.
set -euo pipefail
cd "$(dirname "$0")/.."
VER="${1:-$(cat VERSION)}"
echo "$VER" > VERSION

# Rust workspace version (rust/Cargo.toml [workspace.package]).
sed -i -E "s/^version = \"[0-9]+\.[0-9]+\.[0-9]+\"/version = \"$VER\"/" rust/Cargo.toml

# Flutter version (keep the +build suffix, default +1).
build="$(grep -m1 '^version:' flutter/pubspec.yaml | sed -E 's/.*\+([0-9]+).*/\1/')"
[ "$build" = "$(grep -m1 '^version:' flutter/pubspec.yaml)" ] && build=1
sed -i -E "s/^version: .*/version: $VER+$build/" flutter/pubspec.yaml

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
