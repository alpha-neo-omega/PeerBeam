# Desktop Support & Compatibility Report

PeerBeam's Flutter client targets **Windows, macOS, and Linux** desktop, plus
Android. This document records platform-specific differences and the current
compatibility status of each desktop capability.

## Platform scaffolding

All three desktop platform folders are present and the app is written to run on
each without platform-specific assumptions in shared code:

| Platform | Folder | Status |
|---|---|---|
| Linux | `linux/` | Scaffolded; **build verified** (`flutter build linux`). |
| Windows | `windows/` | Scaffolded (`flutter create --platforms=windows`); build not run in this environment (no Windows toolchain). |
| macOS | `macos/` | Scaffolded; entitlements configured (below); build not run in this environment (no macOS toolchain). |
| Android | `android/` | Existing; see [Android](ANDROID.md). |

> **Build-verification scope.** This report was produced on Linux. Linux desktop
> is built and tested here. Windows and macOS are scaffolded and their shared
> Dart code is the same code that builds on Linux, but their native builds must
> be run on a Windows/macOS host (or CI runner) to be certified — that is the
> one remaining step for a full three-OS release.

## How the code stays portable

- **No platform-specific assumptions in shared code.** Android-only capabilities
  (foreground service, notifications, battery optimization, multicast lock) go
  through `PlatformBridge`; every method is a **no-op off Android**
  (`AndroidBridge` checks `defaultTargetPlatform == android` and returns
  early). So the same controllers run on every platform without `MissingPlugin`
  errors.
- **Desktop-only features are gated** behind an `isDesktop` check
  (`linux || macOS || windows`): drag & drop, the native file picker, and the
  save-directory chooser only activate on desktop; mobile falls back to its own
  flows.
- **Paths** are handled with basename normalization that accepts both `/` and
  `\`, and only file **paths + sizes** are held (never bytes), so behaviour is
  identical across filesystems.

## Capability compatibility matrix

| Capability | Windows | macOS | Linux | Mechanism |
|---|:--:|:--:|:--:|---|
| Drag & drop (send) | ✓ | ✓ | ✓ | `desktop_drop`, gated to desktop |
| File picker (send) | ✓ | ✓ | ✓ | `file_selector` `openFiles()` |
| Save-directory dialog | ✓ | ✓ | ✓ | `file_selector` `getDirectoryPath()` |
| Networked transfer (QUIC) | ✓ | ✓ | ✓ | Rust engine (`peerbeam` CLI/engine) |
| OS notifications | ✗ | ✗ | ✗ | Not implemented on desktop (in-app UI only) |
| System tray | ✗ | ✗ | ✗ | Not implemented |
| Background service | — | — | — | Android-only concept; N/A on desktop |

✓ = implemented (Linux build-verified; Windows/macOS pending a host build).

## Verifications performed

- **Drag & drop** — implemented via `desktop_drop`, gated to desktop, compiles
  into the Linux desktop build; unit-tested (`test/drop_zone_test.dart`).
- **File picker** — `pickFilesToStage()` (`file_selector.openFiles`) wired to
  the Home "Send Files" action; opens the staged-files sheet with the chosen
  files. Compiles into the Linux build; off-desktop it is gated and falls back
  to guidance (tested in `test/desktop_test.dart`).
- **Save dialog** — `pickSaveDirectory()` (`file_selector.getDirectoryPath`)
  wired to Settings → "Save to" (desktop only); updates the save directory.
- **Notifications** — no desktop OS-notification backend yet. Transfer status is
  shown in-app. (Android uses native notifications via the platform bridge.)
- **Tray integration** — not implemented.

> GUI dialogs (picker/save) cannot be driven in a headless test run; they are
> verified by compilation + integration wiring here, and require a manual pass
> on each desktop for full sign-off.

## Platform-specific differences

- **Notifications.** Android shows native + foreground-service notifications.
  Desktop currently surfaces status in-app only. A desktop notifier
  (`local_notifier` / `flutter_local_notifications` desktop) is a follow-up.
- **Background operation.** The Android foreground service, battery-optimization
  exemption, and Wi-Fi multicast lock have no desktop equivalent and are no-ops
  off Android.
- **macOS sandbox.** `macos/Runner/*.entitlements` enable
  `network.client` + `network.server` (P2P QUIC) and
  `files.user-selected.read-write` (picked/dropped files, received files). Files
  the user did not explicitly select are not accessible under the sandbox.
- **Windows firewall.** The first time `receive`/`daemon` binds the transfer
  port, Windows prompts to allow the app through the firewall. Allow it for
  discovery + inbound transfers.
- **File permissions.** Received files are finalized `0600` on Unix
  (Linux/macOS); on Windows default ACLs apply — see [Security](SECURITY.md).
- **Drag & drop** is desktop-only; Android uses the system share sheet
  ([Android](ANDROID.md)).

## Remaining for full desktop certification

1. Run `flutter build windows` and `flutter build macos` on their hosts (or CI).
2. Manual pass of picker/save dialogs and drag & drop on Windows and macOS.
3. (Optional) Desktop OS notifications and system-tray integration.

## Build commands

```bash
flutter build linux            # verified here
flutter build windows          # on a Windows host
flutter build macos            # on a macOS host
flutter test                   # widget + guard tests
```
