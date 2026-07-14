#!/usr/bin/env bash
# Build the release Android APK + AAB. Signing config is read from
# flutter/android/key.properties (see key.properties.example). Without it, the
# build falls back to debug signing (test only).
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p dist

# Build the Rust FFI for Android ABIs into jniLibs so the app never ships a
# stale engine. Requires cargo-ndk + an NDK (ANDROID_NDK_HOME / *_LATEST_HOME /
# *_ROOT). Warns and keeps the committed .so if the toolchain is absent.
echo "== build Rust FFI for Android (arm64-v8a, armeabi-v7a, x86_64) =="
NDK="${ANDROID_NDK_HOME:-${ANDROID_NDK_LATEST_HOME:-${ANDROID_NDK_ROOT:-}}}"
if command -v cargo-ndk >/dev/null && [ -n "$NDK" ]; then
  ( cd rust && ANDROID_NDK_HOME="$NDK" cargo ndk \
      -t arm64-v8a -t armeabi-v7a -t x86_64 \
      -o ../flutter/android/app/src/main/jniLibs \
      build --release -p peerbeam-ffi )
else
  echo "WARN: cargo-ndk or NDK missing — using the committed jniLibs .so (may be stale)"
fi

echo "== build release APK + AAB =="
( cd flutter
  flutter build apk --release
  flutter build appbundle --release
)
cp flutter/build/app/outputs/flutter-apk/app-release.apk dist/ 2>/dev/null || true
cp flutter/build/app/outputs/bundle/release/app-release.aab dist/ 2>/dev/null || true
echo "== done. artifacts in dist/ =="
