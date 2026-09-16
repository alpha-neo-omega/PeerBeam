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
| OS notifications | ✓ | ✓ | ✓ | `flutter_local_notifications` (chat + group messages) |
| System tray / menu bar | ✓ | ✓ | ✓ | `tray_manager` + `window_manager` |
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
- **Notifications** — arriving chat messages raise an OS notification
  (`lib/platform/desktop_notifier.dart`). Transfer status is still in-app only.
  Whether to notify, and what the notification says, are pure functions with
  unit tests (`lib/platform/chat_notifications.dart`,
  `test/chat_notifications_test.dart`); only the delivery call needs a desktop
  session, and it is **build-verified on Linux, unverified on Windows and
  macOS** — the same standing as the tray.
- **Tray integration** — a status icon with live transfers and online devices,
  plus Open / Send files… / Quit. Windows draws it in the notification area,
  macOS in the menu bar, Linux via Ayatana's app-indicator. The menu's *content*
  is a pure function (`lib/platform/tray_model.dart`) with unit tests
  (`test/tray_model_test.dart`); the plugin call site cannot be driven headless
  and needs a manual pass per desktop.

  Closing the window can leave PeerBeam running in the tray, **off by default**
  (Settings → "Keep running when I close it"). Off is the safe default because a
  person who closes a window generally believes they closed the program, and it
  would otherwise keep receiving files. Quit is always in the tray menu.

  **Linux needs `libayatana-appindicator3-1` at runtime** (`-dev` to build). It
  is a hard dependency — a NEEDED entry on the binary, not an optional feature —
  so the `.deb`, `.rpm` and PKGBUILD all declare it.

> GUI dialogs (picker/save) cannot be driven in a headless test run; they are
> verified by compilation + integration wiring here, and require a manual pass
> on each desktop for full sign-off.

## Platform-specific differences

- **Notifications.** Android keeps its own path — the foreground service and
  hand-written channels in `android/` — because that is what survives the app
  being backgrounded. Desktop uses `flutter_local_notifications`. So:
  - **macOS** asks for notification permission the first time a message would
    raise one, not at launch. A refusal is remembered for that session.
  - **Clicking a desktop notification opens that conversation.** On Android the
    notification's content intent opens the app, as it always has.
  - Transfer progress and completion still notify on Android only.
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
3. Confirm notifications actually appear on Windows and macOS (Linux verified).

## Build commands

```bash
flutter build linux            # verified here
flutter build windows          # on a Windows host
flutter build macos            # on a macOS host
flutter test                   # widget + guard tests
```
