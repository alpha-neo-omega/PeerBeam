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

## Chat attachments

### A typed attach menu

The chat composer's attach button opens a bottom sheet offering **Document**
(no filter — today's picker, kept), **Photos & videos**, or **Audio**, instead
of one undifferentiated "any file" picker. No camera capture: there is no
camera dependency in this project.

The chosen kind is a small platform-layer enum (`AttachKind`, in
`platform/desktop_files.dart`) rather than a raw MIME string threaded down
from the UI — the composer names *what* the user wants, and the platform
layer owns *how* that becomes an OS filter:

- **Desktop** (`file_selector`): each kind maps to an `XTypeGroup` carrying
  **both** `mimeTypes` and `extensions`. Extensions are required alongside
  MIME types because a Linux GTK picker filters by extension and ignores MIME
  — a mimeTypes-only group silently shows nothing there.
- **Android**: the kind's MIME types (e.g. `image/*, video/*`) are passed
  through the `pickFiles` method channel and set as `EXTRA_MIME_TYPES` on the
  `ACTION_OPEN_DOCUMENT` intent (`type` stays the wildcard alongside it —
  `EXTRA_MIME_TYPES` is what actually filters when present). The channel
  argument is optional with a wildcard default, so no other caller of the
  native picker needed to change.

A file the filter excludes is never offered in the first place — never picked
and then rejected afterwards. Everything past the choice is unchanged: the
picker is still multi-select, the Android path still streams into cache and
returns paths only (never bytes), and every picked file still becomes its own
chat message, exactly as before this menu existed.

`test/chat_screen_test.dart` pins the menu's three choices and that each hands
the picker its own kind; `test/desktop_files_test.dart` pins the kind → filter
mapping on both platforms.

## Approving several transfers at once

When **two or more** inbound transfers are awaiting approval, the Transfers
screen shows a banner above the list — `N items waiting for approval`, with
**Select**, **Decline all** and **Accept all**. Below two it stays hidden: over
a single card its own Accept is already the shortest path, and a banner there is
just a second button saying the same thing. Every card keeps its own Decline /
Accept / Trust; the banner is an addition, never a replacement.

**Items, not files.** A waiting transfer can be a whole folder, so the banner
counts what is queued for a decision rather than claiming to know what each one
holds.

### Answering only some of them

**Select** switches the banner to selection mode: a checkbox appears on every
*awaiting-approval* card (the whole card is the hit target, not just the
checkbox), the heading becomes `N of M selected`, and the actions become
**Cancel** / **Decline** / **Accept** — now answering exactly the checked ids
via `acceptOnly`/`declineOnly`. A card that is not awaiting approval has nothing
left to decide, so it never gains a checkbox.

Three properties keep this from becoming a second, weaker consent path:

- **Selection mode has exactly the same consent as Accept all** — `accept`,
  never `acceptTrust`. While selecting, the per-card Decline/Accept/**Trust**
  cluster is hidden so there is one action path on screen rather than two, and
  **Trust** is never reachable as part of a batch: there is no "Trust selected".
- **The selection is narrowed to liveness on every build.** Checked ids are
  intersected with what is still awaiting approval, so a transfer that settles
  under an open selection drops out of the count and is never handed to the
  engine. A batch in flight disables every control, including Select/Cancel, so
  a result cannot land in a mode the user already left.
- **Selection mode ends when there is no batch left.** It is left when the batch
  lands, when Cancel is tapped, and when the banner itself goes away — that last
  one matters because inbound transfer ids come from the *sender*, so a
  selection left set could otherwise pre-check a later transfer that happened to
  reuse an id.

### Both paths

Two properties are load-bearing for the banner as a whole:

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
  `no pending transfer <id>`. Every entry point (`acceptAll`/`declineAll` and
  `acceptOnly`/`declineOnly`) falls through to one loop that **awaits** each
  decision and returns a verified tally (`requested`/`settled`/`gone`/`failed`,
  which always sum), instead of pushing each refusal onto the error stream —
  that would fire one snackbar per casualty and read as breakage. One line comes
  back: `Accepted 3 items`, `Accepted 3 of 5 — 2 were no longer waiting`, or
  `None were still waiting`. The per-card `accept`/`reject` are untouched and
  stay fire-and-forget, so a card tap is still instant.

`test/transfers_screen_test.dart` pins the visibility rule, the accept-not-trust
call log for both paths, the scope, selection mode's entry/exit and liveness
narrowing, and each report; `test/data/repository_test.dart` pins the same at
the repository seam.

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
