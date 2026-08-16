# UI (Flutter)

The v2 Flutter client — a modern, responsive Material 3 shell that mirrors
v1's screens (Home, Transfers, History, Settings) with **no new features**.
It renders against a mock state layer for now; the Rust engine wires in at the
FFI milestone. This deliberately fixes the issues raised in the UI audit.

## Structure

```
lib/
  main.dart                 root: AppState + router, theme-only rebuilds
  app/
    theme.dart              Material 3 seed → light/dark; motion + breakpoint tokens
    router.dart             go_router StatefulShellRoute.indexedStack (state-preserving)
    shell.dart              responsive nav: bar / rail / extended rail
  state/
    models.dart             view models (Device, Transfer, HistoryItem)
    stores.dart             per-domain ChangeNotifiers + sample data
    app_scope.dart          InheritedWidget exposing AppState
  widgets/                  StatusDot, DeviceTile, QuickAction, EmptyState, Appear, …
  features/{home,chats,transfers,history,settings}/
```

## How the audit findings were addressed

| Audit finding | Resolution |
|---|---|
| N1 tabs lose state on switch | `StatefulShellRoute.indexedStack` keeps every tab alive |
| N2 no declarative routing / deep links | `go_router` with URL-addressable branches |
| N3 no back handling | Router-integrated navigation (system back works) |
| A1 zero Semantics | `StatusDot`, `DeviceTile`, `QuickAction` carry semantic labels / `button` roles; `MergeSemantics` on tiles |
| A2 icon buttons without tooltips | Every `IconButton` has a tooltip |
| A4 status by colour only | `StatusDot` exposes "Online/Offline" to a11y and is paired with text |
| R1 content doesn't reflow / no max width | Content capped to a readable width; device list is a responsive `SliverGrid` (columns by width) |
| R2 orientation locked | No orientation lock |
| P1 whole-tree rebuilds on god-provider | Per-domain stores; each screen `AnimatedBuilder`s only the store it needs |
| P3 non-builder ListViews | `ListView.builder` / `SliverGrid.builder` everywhere |

## Modern / native touches

- Material 3 throughout, tonal light + dark from one seed; system/light/dark
  switch (segmented control in Settings).
- Adaptive navigation: bottom bar < 600px, rail < 1000px, extended rail
  beyond — one shell, three layouts.
- Motion: shared duration/curve tokens; `SliverAppBar.large` collapsing
  header, staggered list entrances (`Appear`), animated progress bars,
  pulsing presence dots, animated scan toggle.
- Platform-adaptive controls (`Switch.adaptive`) and a transfer-count `Badge`
  on the nav.

## Drag & drop (desktop only)

`DropZone` (in `features/send/`) wraps the whole content area. On desktop
(Linux/macOS/Windows) it accepts dropped files; on mobile/web it is a
transparent passthrough. Dropped items are **staged by path + size only** —
never read into memory — so dropping many files or multi-GB files is instant.
A dashed, tinted `DropOverlay` fades/scales in while dragging, then the
staged-files sheet opens for review (per-file remove, running total). Staging
lives in a pure `StagingStore` (dedup by path), unit-tested independently of
any native drag.

## Approving several transfers at once

When **two or more** inbound transfers are awaiting approval, the Transfers
screen shows a banner above the list — `N files waiting for approval`, with
**Decline all** and **Accept all**. Below two it stays hidden: over a single
card its own Accept is already the shortest path, and a banner there is just a
second button saying the same thing. Every card keeps its own Decline / Accept
/ Trust; the banner is an addition, never a replacement.

Two properties are load-bearing:

- **Accept-once. The banner never trusts.** `acceptTrust` grants a device
  persistent auto-accept for everything it sends from then on — materially
  stronger than approving the batch on screen — so it stays a deliberate,
  per-device choice on the card. There is no "Trust all", and nothing about a
  batch decision is remembered for next time (invariant I6: consent is explicit
  and per-act). The scope is equally narrow: `TransferRepository.awaitingApproval`
  is **inbound** transfers in `pending` only, so an outbound send (never anyone's
  to approve) and an already-running, paused, completed or failed transfer are
  untouchable from here.
- **It reports what actually happened, not what it assumed.** Between the render
  and the tap a transfer can time out, its sender can give up, or the user can
  answer it from its own card — the engine then refuses that decision with
  `no pending transfer <id>`. `acceptAll`/`declineAll` **await** each decision
  and return a verified tally (`requested`/`settled`/`gone`/`failed`) instead of
  pushing each refusal onto the error stream, which would fire one snackbar per
  casualty and read as breakage. One line comes back: `Accepted 3 files`,
  `Accepted 3 of 5 — 2 were no longer waiting`, or `None were still waiting`.
  The per-card `accept`/`reject` are untouched and stay fire-and-forget, so a
  card tap is still instant.

`test/transfers_screen_test.dart` pins the visibility rule, the accept-not-trust
call log, the scope, and each report; `test/data/repository_test.dart` pins the
same at the repository seam.

## UX polish pass

A dedicated pass over the whole app (no new features — experience only):

- **Accessibility — reduced motion.** All decorative animation now respects the
  OS "reduce motion" setting via `AppMotion.enabled(context)` /
  `AppMotion.duration(context, …)`. The presence-dot pulse stops, list
  entrance stagger (`Appear`) resolves instantly, the empty-state icon skips its
  scale-in, and transfer progress jumps rather than tweening. Screen-reader
  labels/semantics were already in place (device tiles, quick actions, status).
- **Keyboard shortcuts.** Desktop navigation with **Ctrl/⌘ + 1–5** to jump to
  Home / Chats / Transfers / History / Settings (`CallbackShortcuts` at the
  shell). The digits are **positional** — they index the nav order, not named
  screens — so inserting Chats at position 2 moved Transfers to Ctrl+3, History
  to Ctrl+4 and Settings to Ctrl+5. `test/ux_test.dart` pins the whole mapping,
  and separately pins that each destination opens the screen its own label
  names (the nav order and the router's branch order must not drift apart).
- **Destructive-action confirmation.** "Clear history" now asks for confirmation
  ("cannot be undone") before wiping records.
- **Copy.** User-facing placeholder text is friendly ("… is coming soon")
  instead of developer wording; snackbars replace the previous one instead of
  stacking.
- **Visual hierarchy.** Offline device tiles are dimmed so reachable peers stand
  out.
- **Theme consistency.** The online/success green is a single semantic token
  (`AppColors.online`) rather than a per-widget literal; motion tokens
  (`AppMotion`) remain the single source for durations/curves.

These are covered by widget tests in `test/ux_test.dart` (reduced-motion pulse,
keyboard tab switch) plus the existing regression tests.

## Verification

`flutter analyze` — no issues. `flutter test` — all pass (smoke, drop-zone,
staging, platform, regression, desktop, and UX polish tests). (Native
desktop/Android builds require their platform toolchains, not run here.)

## Not yet

Engine wiring (FFI) — all actions currently show a placeholder. App-level
transfer-approval handling, QR pairing, and localization land with the bridge.
