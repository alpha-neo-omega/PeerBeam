# PeerBeam v0.2.4 — Beta

macOS desktop, wired up. The GUI now bundles the Rust engine correctly, so the
macOS app actually runs.

## Highlights

### macOS GUI now launches
- The app loaded the engine (`libpeerbeam_ffi.dylib`) by bare name, which macOS
  `dlopen` never resolves from an app bundle — and nothing built or embedded the
  dylib in the `.app` at all. The macOS GUI therefore failed to start the engine.
- Now the engine is built as a **universal binary** (x86_64 + arm64), embedded
  in `PeerBeam.app/Contents/Frameworks` with an `@rpath` install id, and the app
  loads it by an executable-relative path. One DMG runs natively on **Intel and
  Apple Silicon**.

## Downloads
- **macOS**: `PeerBeam-0.2.4.dmg` (universal). It is **unsigned/un-notarized**
  (signing secrets are not configured), so Gatekeeper blocks it on first open.
  After copying PeerBeam to Applications, clear the quarantine flag:
  ```
  xattr -dr com.apple.quarantine /Applications/PeerBeam.app
  ```
  then open it. (Alternatively: right-click the app → Open → Open.)
- Linux (`.deb`, `.tar.gz`), Windows (portable `.zip`), Android (`.apk`/`.aab`),
  and CLI binaries for all three desktop OSes ship as before.

## Gate
- 284 Rust tests, 56 Flutter tests, `clippy -D warnings` clean, `flutter
  analyze` clean.

_The macOS DMG is built in CI but has not been run on Apple hardware by the
maintainers this cycle; please report launch issues. Signed/notarized macOS and
Windows installers still pend signing secrets (see docs/RELEASE.md)._
