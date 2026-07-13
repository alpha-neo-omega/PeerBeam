# Building PeerBeam

## Prerequisites
- **Rust** ≥ 1.80 (`rustup`), **Flutter** (stable).
- Linux desktop: `ninja-build`, `libgtk-3-dev`, `librsvg2-bin` (icons);
  optional packagers `dpkg-dev`, `rpm`, `appimagetool`.
- Windows: Visual Studio Build Tools; MSIX via the bundled `msix` dev-dep.
- macOS: Xcode; optional `create-dmg`.
- Android: JDK 17 + Android SDK.

## Version
One source of truth: the `VERSION` file. Sync everything:
```
scripts/set-version.sh 0.3.0     # updates rust/Cargo.toml + flutter/pubspec.yaml
```
The engine reports its version over FFI (`pb_version_json`, from `CARGO_PKG_VERSION`);
Flutter uses the pubspec version. Keep them in lock-step via the script.

## Engine bridge
```
scripts/build-ffi.sh release     # builds peerbeam-ffi (cdylib) for the host
```
The Flutter platform glue bundles the resulting library:
- **Linux:** `linux/CMakeLists.txt` installs `libpeerbeam_ffi.so`.
- **Windows:** `scripts/package-windows.ps1` copies `peerbeam_ffi.dll` beside the runner.
- **macOS:** link/copy the `.dylib` into the bundle (script), loaded via the SDK.
- **Android:** place `libpeerbeam_ffi.so` under `android/app/src/main/jniLibs/<abi>/`
  (cross-compile with `cargo-ndk`; see RELEASE.md).

## Per-platform packaging
```
scripts/package-linux.sh      # tar.gz (+ .deb/.rpm/AppImage if tools present)
scripts/package-windows.ps1   # MSIX (on Windows)
scripts/package-macos.sh      # signed, notarization-ready DMG (on macOS)
scripts/package-android.sh    # release APK + AAB
```
Artifacts land in `dist/`. Each script builds the engine + Flutter in `--release`
first, so builds are self-contained.

## Reproducibility
- Pin toolchains (rustup + Flutter channel), build `--release`, no local paths in
  artifacts. Record `rustc --version` + `flutter --version` with each release.
- CI (`.github/workflows/release.yml`) is the canonical build environment.
