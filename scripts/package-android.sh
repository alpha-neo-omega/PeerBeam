#!/usr/bin/env bash
# Build the release Android APK + AAB. Signing config is read from
# flutter/android/key.properties (see key.properties.example). Without it, the
# build falls back to debug signing (test only).
#
# The APK is the artifact you install to test; the AAB is only for the Play
# Store. They are built and reported independently so a store-only problem can
# never cost you the testable APK. Set PEERBEAM_SKIP_AAB=1 to skip the bundle.
set -euo pipefail
cd "$(dirname "$0")/.."
# In CI the tag is the source of truth; VERSION is the local fallback.
VER="${GITHUB_REF_NAME:-}"
VER="${VER#v}"
[ -n "$VER" ] || VER="$(cat VERSION)"
mkdir -p dist

# Build the Rust FFI for Android ABIs into jniLibs so the app never ships a
# stale engine. Requires cargo-ndk + an NDK. The NDK is taken from the usual
# env vars, else discovered under the SDK — an unset env var is not evidence
# that the NDK is absent, and silently shipping a months-old committed engine
# is the failure this step exists to prevent.
echo "== build Rust FFI for Android (arm64-v8a, armeabi-v7a, x86_64) =="
NDK="${ANDROID_NDK_HOME:-${ANDROID_NDK_LATEST_HOME:-${ANDROID_NDK_ROOT:-}}}"
if [ -z "$NDK" ]; then
  for sdk in "${ANDROID_HOME:-}" "${ANDROID_SDK_ROOT:-}" "$HOME/Android/Sdk"; do
    [ -n "$sdk" ] && [ -d "$sdk/ndk" ] || continue
    # Highest version wins; `sort -V` orders 28.2.x above 9.x correctly.
    NDK="$(find "$sdk/ndk" -maxdepth 1 -mindepth 1 -type d | sort -V | tail -1)"
    [ -n "$NDK" ] && break
  done
fi
if command -v cargo-ndk >/dev/null && [ -n "$NDK" ]; then
  echo "   NDK: $NDK"
  ( cd rust && ANDROID_NDK_HOME="$NDK" cargo ndk \
      -t arm64-v8a -t armeabi-v7a -t x86_64 \
      -o ../flutter/android/app/src/main/jniLibs \
      build --release -p peerbeam-ffi )
else
  echo "WARN: cargo-ndk or NDK missing — falling back to the COMMITTED jniLibs .so."
  echo "WARN: that engine is only as fresh as its last commit; if it predates the"
  echo "WARN: feature you are testing, the app will not contain it. Install"
  echo "WARN: cargo-ndk and an NDK, or set ANDROID_NDK_HOME, to build it here."
  for so in flutter/android/app/src/main/jniLibs/*/libpeerbeam_ffi.so; do
    [ -e "$so" ] && echo "WARN:   using $so ($(date -r "$so" '+%Y-%m-%d %H:%M'))"
  done
fi

# ---- APK: the artifact you install. Copied before anything else runs. ----
echo "== build release APK =="
( cd flutter && flutter build apk --release )
APK="dist/peerbeam-${VER}-android.apk"
cp -f flutter/build/app/outputs/flutter-apk/app-release.apk "$APK"
echo "OK  $APK"

# ---- AAB: Play Store only. Never allowed to cost us the APK above. ----
aab_status=0
if [ "${PEERBEAM_SKIP_AAB:-0}" = "1" ]; then
  echo "== skip AAB (PEERBEAM_SKIP_AAB=1) =="
else
  echo "== build release AAB =="
  if ( cd flutter && flutter build appbundle --release ); then
    AAB="dist/peerbeam-${VER}-android.aab"
    cp -f flutter/build/app/outputs/bundle/release/app-release.aab "$AAB"
    echo "OK  $AAB"
  else
    aab_status=1
    echo "WARN: the app bundle failed. The APK above is unaffected and installable."
    # `flutter build appbundle --release` verifies stripping by running
    # apkanalyzer, which ships in the SDK's cmdline-tools. Without it the check
    # cannot run, so Flutter reports a strip failure that is really a missing
    # tool — worth naming, because the message itself points at the toolchain.
    # Look for the binary rather than parsing `flutter doctor`: that is slow,
    # and its human-readable output is not a stable interface to match on.
    SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Android/Sdk}}"
    if [ -n "$(find "$SDK/cmdline-tools" -name apkanalyzer -type f 2>/dev/null | head -1)" ]; then
      echo "WARN: see the build output above for the cause."
    else
      echo "WARN: cause: the Android SDK's cmdline-tools component is missing, so"
      echo "WARN: Flutter cannot run apkanalyzer to verify that debug symbols were"
      echo "WARN: stripped, and treats the unverifiable bundle as a failure."
      echo "WARN: Install it (Android Studio > SDK Manager > SDK Tools >"
      echo "WARN: 'Android SDK Command-line Tools'), then re-run."
    fi
  fi
fi

echo "== done. artifacts in dist/ =="
# Surface the bundle failure to CI *after* the APK has been delivered.
exit "$aab_status"
