#!/usr/bin/env bash
# Build the Rust engine bridge (peerbeam-ffi) for the host, release profile.
# The Flutter platform build glue bundles the resulting shared library.
set -euo pipefail
cd "$(dirname "$0")/.."
profile="${1:-release}"
echo "building peerbeam-ffi ($profile)…"
if [ "$profile" = "release" ]; then
  cargo build --manifest-path rust/Cargo.toml --release -p peerbeam-ffi
else
  cargo build --manifest-path rust/Cargo.toml -p peerbeam-ffi
fi
echo "built: rust/target/$profile/"
