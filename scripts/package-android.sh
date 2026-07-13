#!/usr/bin/env bash
# Build the release Android APK + AAB. Signing config is read from
# flutter/android/key.properties (see key.properties.example). Without it, the
# build falls back to debug signing (test only).
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p dist
echo "== build release APK + AAB =="
( cd flutter
  flutter build apk --release
  flutter build appbundle --release
)
cp flutter/build/app/outputs/flutter-apk/app-release.apk dist/ 2>/dev/null || true
cp flutter/build/app/outputs/bundle/release/app-release.aab dist/ 2>/dev/null || true
echo "== done. artifacts in dist/ =="
