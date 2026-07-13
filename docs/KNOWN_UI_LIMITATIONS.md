# Known UI Limitations (M6)

Verified-limited by environment (headless: no rendered pixels / FPS / screen
reader / non-Linux desktop here):
- Visual balance on **tablets / ultra-wide** and **native feel on Windows/macOS**
  are code-level, not eyeballed.
- **Screen-reader** output (TalkBack/VoiceOver/Orca) not run — Semantics added
  but not heard here.
- **FPS / jank** not profiled on a device.

Not implemented (out of M6 scope — would be new features):
- Multi-selection, right-click **context menus**, copy/paste/delete on lists.
- **Desktop OS notifications** + system tray (Android notifications work).
- **In-app search** (Home search action is a placeholder), **QR pairing**.
- **Clipboard** UI + **Settings** wired to the M3 engine settings (still local).
- Screenshots in the README (needs a running desktop/mobile session).

Cosmetic/minor:
- "Coming soon" quick actions (Search/QR/Clipboard) remain until their features
  land — honest placeholders, not fake data.
