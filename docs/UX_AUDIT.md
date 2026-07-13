# UX Audit (M6)

Scope: Flutter presentation layer only. No new features; no engine/FFI/network
changes. **Verification honesty:** this pass is headless — code + `flutter
analyze` + `flutter test` are verified; rendered pixels, FPS, and screen-reader
output are *not* eyeballed here and are marked "code-level".

Status key: ✅ done+tested · 🎨 code-level (needs a device to eyeball) · ➖ prior
pass · 🔧 needs hardware/device to verify.

| Phase | Status | Notes |
|---|---|---|
| 1 Visual consistency | ➖🎨 | Central tokens already exist: `AppMotion` (durations/curves), `AppColors.online`, `CardTheme`/`listTileTheme` (radius 20/16), consistent 12/16 spacing. No stray colors/radii found. |
| 2 Material 3 polish | ➖ | M3 throughout (`ColorScheme.fromSeed`, NavigationBar/Rail, FilledButton, adaptive switches, floating snackbars, stadium indicators). Hover/pressed/focus come from M3 ink defaults. |
| 3 Animations | ➖ | `Appear` stagger, animated progress tween, `AnimatedSwitcher` scan icon, empty-state scale — all **reduced-motion aware** (`AppMotion.duration`). No excessive motion added. |
| 4 Responsive | ➖🎨 | `Breakpoints` (compact/medium) drive bottom-bar↔rail↔extended-rail; `contentMaxWidth` caps line length; device grid `SliverGridDelegateWithMaxCrossAxisExtent`. Regression tests cover long-name overflow. Ultra-wide/tablet not eyeballed. |
| 5 Accessibility | ✅ | Added merged **Semantics** to transfer cards (announces "Sending file to peer, N percent, status"). Device tiles/quick-actions/status already labelled. Reduced-motion respected. Touch targets ≥48dp (icon buttons/cards). |
| 6 Keyboard & desktop | ➖ | Ctrl/⌘+1–4 tab nav; drag-and-drop (desktop); native file picker + save dialog. (Multi-select/context-menus = not implemented — see limitations.) |
| 7 Error experience | ✅ | **New** `friendlyError`/`friendlyErrorForCode`: engine errors → clear, actionable, non-technical text; **no `quic`/FFI/exception leakage** (tested). Wired into send flows + the transfer-error snackbar stream. |
| 8 Loading & empty states | ➖🎨 | Animated `EmptyState` for no-devices/no-transfers/no-history; scan toggle shows state. "Connecting/receiving" surfaced via transfer status text. |
| 9 Notifications | ➖ | Android foreground-service notification reflects active count + reception; single persistent service notification (no spam). Desktop OS notifications = not implemented. |
| 10 Settings | ➖ | Grouped (Device/Transfers/…); destructive "Clear history" confirms; save-dir picker (desktop). Engine-backed settings land when the Flutter settings repo is wired to the M3 FFI. |
| 11 Performance perception | ➖ | Per-store `AnimatedBuilder` scoping (a change in one domain never rebuilds the tree); lazy `ListView.builder`; immediate optimistic scan toggle. |
| 12 Cross-platform UX | 🔧 | Material adapts per platform; `isDesktop` gating for DnD/picker. Native feel on Windows/macOS not eyeballed here. |
| 13 First-run walkthrough | 🎨 | Flow: launch → auto-discovery → tap device → pick file → send → snackbar; errors friendly; history reactive. Friction points logged in DESIGN_DECISIONS. |

## Verified this pass
- Friendly, leak-free error text (unit-tested).
- Transfer-card screen-reader semantics.
- `flutter analyze` clean, `flutter test` green (35), no regressions.
