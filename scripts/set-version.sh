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

echo "version set to $VER (flutter build $build)"
grep -m1 '^version' rust/Cargo.toml
grep -m1 '^version:' flutter/pubspec.yaml
