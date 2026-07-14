# UI Redesign Report — Material 3 Production Polish

Scope: **UI/UX only.** No features added or removed; no changes to business
logic, networking, Rust, FFI, repositories, application state, architecture,
navigation structure, workflows, or APIs. No new dependencies.

Verification after every group: `flutter analyze` (clean) and `flutter test`
(35 passing). Committed per logical group.

---

## Screens redesigned

| Screen | Changes |
| --- | --- |
| **Home** | Token-based spacing; saved devices rendered as hover-lift cards with gradient avatars (matching device tiles) instead of plain `ListTile`s; filled-tonal "add" affordance; dialog gaps via `Gap`; grid extent adjusted for the larger tiles. |
| **Transfers** | Redesigned transfer cards: gradient state-tinted avatar, prominent percent badge, state-colored progress bar, and a status pill (success / warning / error / primary by state). |
| **History** | Rows became gradient-avatar cards (state-tinted success/error) with a tidy title/meta layout. |
| **Settings** | Token-ized spacing throughout; consistent card/group rhythm. |
| **Send (staged sheet)** | Token-ized paddings and layout. |
| **Drop overlay** | Reviewed — already polished; left intact. |
| **Shell / nav** | Rail brand mark uses tokens + a diagonal brand gradient; nav bar/rail styling centralized in the theme. |
| **Empty states** | Tonal gradient icon badge, stronger title hierarchy. |
| **Dialogs / snackbars / bottom sheets** | Styled centrally via the theme (radius, elevation, drag handle, tonal surfaces). |

## Design system

Central tokens added to `lib/app/theme.dart` (backward-compatible public API):

- **Spacing** — `AppSpace` (4 / 8 / 12 / 16 / 20 / 24 / 32 / 40) on an 8-pt grid.
- **Radius** — `AppRadius` (8 / 12 / 16 / 20 / 28 / full).
- **Elevation** — `AppElevation` (level0–3, M3 tonal).
- **Icons** — `AppIcons` (18 / 22 / 28 / 40).
- **Motion** — existing `AppMotion` (durations, curves, reduced-motion helpers).
- **Colors** — `AppColors` extended with `success` / `warning` beside `online`.
- **Breakpoints** — existing window-size classes.
- **`Gap`** — a square spacer that works in both `Row` and `Column`.

`ThemeData` now centrally styles: AppBar, cards, filled/elevated/outlined/text
buttons, segmented buttons, chips, list tiles, inputs, dialogs, bottom sheets,
snackbars, dividers, tooltips, nav bar and nav rail — plus refined typography
weight/tracking on display/headline/title/label roles (M3 sizes kept, so
layouts stay stable).

## Components improved

`QuickAction`, `DeviceTile` (+ avatar, reach chips), `EmptyState`,
`SectionHeader`, transfer card, saved-device card, history row, status pill.
New shared `HoverScale` gives tappable cards a subtle desktop hover lift.

## Accessibility

- Preserved all existing `Semantics` labels (device tiles, transfer cards,
  status dots, quick actions) — status never conveyed by color alone (text +
  chip accompany every state color).
- Reduced-motion respected everywhere: `HoverScale`, `Appear`, `EmptyState`,
  `StatusDot`, progress animations all collapse when `disableAnimations` is set.
- Touch targets remain ≥48dp (icon buttons, quick actions, cards).
- Tooltips on all icon-only actions.

## Responsive

- Content width capped via `ContentPane` / `Breakpoints.contentMaxWidth`.
- Nav adapts by width: bottom bar (compact) → rail (medium) → extended rail.
- Nearby devices use a max-cross-axis-extent grid (phones: 1 col, wider: N).
- Text uses `maxLines` + `ellipsis` to avoid overflow on narrow screens.

## Performance

- Implicit animations only; no new controllers beyond the existing ones.
- `HoverScale` is pointer-only (`MouseRegion`) — no cost on touch.
- Store-scoped `AnimatedBuilder`s left intact, so redesigns didn't widen
  rebuild scopes. No new dependencies → no bundle/startup impact.

## Design decisions

- **Elevate, don't restructure** — kept the M3 foundation and navigation; the
  work is tokens + consistency + component polish.
- **Gradients as accent, not noise** — subtle two-stop tints on avatars/badges
  for depth without clutter.
- **State color is UI-side** — transfer state→color kept in the screen, not the
  shared model, so `state/models.dart` stays presentation-free.
- **Dark-first, light-equal** — all styling derives from the seed's tonal
  scheme, so both brightnesses stay consistent.

## Follow-up features (approved after the redesign)

These go beyond pure styling; each was explicitly requested and approved.

| Feature | Implementation | Dependency |
| --- | --- | --- |
| **Transfer speed / ETA** | Engine already emits `current_speed` / `eta_secs`; threaded through the `Transfer` view model (`speedBps` / `etaSecs` + `formatSpeed` / `formatEta`). Card shows `done/total · speed · ETA` while transferring. | none |
| **Device search** | Home search action opens a native `SearchDelegate` over discovered devices (filter by name, tap to send). | none |
| **Brand mark** | Custom-painted `PeerBeamMark` (concentric beam waves on the brand gradient) + `BrandLockup`, replacing the bolt glyph in the nav rail. | none (CustomPainter) |
| **QR share / scan** | `qr_flutter` renders a saved device's address (`peerbeam://add?name&host&port`); `mobile_scanner` scans a peer's QR to save it. Scan is mobile-only (camera); desktop gets a hint. | `qr_flutter`, `mobile_scanner` |
| **Send clipboard** | Dart reads the OS text clipboard, writes a temp `.txt`, sends it to a chosen online device via the existing transfer path. | none |

### Deviation from CLAUDE.md — QR

CLAUDE.md's UX section states "No QR codes. No pairing codes." QR share/scan was
added on explicit user approval. It is scoped as **address sharing** (encode/scan
a device's `host:port`), not cryptographic pairing, and does not replace
zero-config discovery — it is an optional convenience for reaching by-address
peers (Tailscale / headless). Recorded here so the deviation is intentional and
traceable.

### Known limitations

- **Clipboard** and **QR scan → save** target picking currently use
  *discovered-online* devices (LAN/mDNS). Over Tailscale-only / cellular there
  are no discovered devices, so clipboard send needs the peer on the same
  network. (QR *scan* saves a by-address device, which then works over
  Tailscale for file send.) Extending the clipboard picker to saved-by-address
  devices is a candidate follow-up.
- **Rust cannot access the Android clipboard**; the OS clipboard read stays in
  Dart by necessity.

## Remaining visual improvements (future, not done here)

- Container-transform transition from a device/saved card into its send flow.
- A custom brand mark asset for the rail (currently a gradient bolt glyph).
- Speed / ETA display on transfer cards (needs those fields surfaced on the
  `Transfer` view model — a state change, so out of this UI-only scope).
- Search UI (the action is currently a "coming soon" placeholder — a feature,
  not styling).
