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

**Two targets, one drop.** `desktop_drop` registers every mounted `DropTarget`
against the same native window and delivers a drop to *all* of them — a nested
target does **not** shadow the one above it. Since `DropZone` wraps the whole
navigation shell, an open conversation always has two targets stacked over it,
and without arbitration one drop was answered twice: sent to the peer *and*
staged for the Send flow, with two overlays lit at once. So `DropZone`
publishes a claim register (`DropClaims`) to everything below it, and stands
down entirely — no handler, no overlay — for as long as a claim is held. It is
a counted `ValueNotifier<int>` rather than a flag, so two claimants overlapping
for a frame (one screen mounting as another releases) cannot re-enable the
outer zone early. What may claim it, and when, is
[Who owns a drop](#who-owns-a-drop).

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
  **both** `mimeTypes` and `extensions`, because the three backends disagree
  about which one they read. `file_selector_windows` builds its filter spec
  from `extensions` alone and never looks at `mimeTypes`; `file_selector_macos`
  maps each MIME through `UTType(mimeType:)` and `compactMap`s away the nils,
  and `UTType(mimeType: "image/*")` *is* nil, so a wildcard silently drops.
  Linux is the one that needs neither list on its own —
  `file_selector_linux` adds both `add_pattern` and `add_mime_type`, so it
  honours `image/*`. So the extensions exist for **Windows and macOS**.
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

### Drag & drop into a conversation

Dropping files onto an open chat sends them straight to that peer — the same
per-file fan-out attach already does, with no confirmation step. Desktop only,
like the Send flow's own drop zone: `ChatDropZone`
(`features/chat/chat_drop_zone.dart`) is a transparent passthrough on
mobile, and shows the same dashed `DropOverlay` while dragging.

**Reused, not duplicated.** The `XFile` → staged-entry walk (metadata only —
never a read — with folder detection and a name fallback) is one shared
function, `collectDroppedFiles` in `features/send/drop_zone.dart`; the Send
flow's own `DropZone` and `ChatDropZone` both call it. The only difference is
what happens with the result: the Send flow opens the staged-files sheet,
`ChatDropZone` sends into the open conversation.

**Folders are refused, with the reason named.** A chat file message carries
one file, and the engine's `prepare_file_send` rejects a directory outright —
so a dropped folder produces a snackbar naming it and pointing at "Send
folder" on Home, rather than a silent no-op or an engine error surfacing
later. A drop mixing files and folders sends the files and reports every
skipped folder in one message; it does not abort the files that came with it.

#### Who owns a drop

`ChatDropZone` claims the shell's [drop register](#drag--drop-desktop-only)
while its conversation is **visible** — and visible is not the same as mounted.
The shell is a `StatefulShellRoute.indexedStack`, which keeps every navigation
branch mounted and merely takes the inactive ones offstage, while
`desktop_drop` decides who to notify from `renderBox.paintBounds.contains(…)`
alone — a gate an offstage `IndexedStack` child still passes, because it is
laid out at full size. A claim scoped to *mounted* therefore let a conversation
left open on Chats answer a drop made on Home: the file went straight to that
peer instead of being staged for the Send flow, with no prompt and no way back.

Visibility is two signals, because there are two ways to stop being on screen.
go_router mounts each branch as `Offstage(offstage: !isActive, child:
TickerMode(enabled: isActive, …))`, so `TickerMode` answers the branch
question; a route pushed *on top of* the conversation inside the same branch
leaves `TickerMode` true and is caught by `ModalRoute`'s `isCurrent` instead.
Both are inherited-widget lookups, which is what makes them reactive — the zone
reconciles again the moment either changes.

**The register is re-resolved on every reconcile, not bound once.** `AppShell`
places `DropZone` in a different slot below and above `Breakpoints.compact`, so
dragging the window across 600px destroys `_DropZoneState` and the notifier it
owns, while the branch Navigator's GlobalKey carries the open conversation into
the replacement. A register that has been **replaced** is dropped rather than
released: decrementing it is a use-after-dispose, and the count it would
correct is going away with its owner. Only a claim on the register still in
scope is ever released — visibility going false, or the screen being disposed.

**Ownership is asserted twice, deliberately.** The chat's own `DropTarget`
takes `enable: visible` *and* its `onDragDone` refuses when it is not visible,
so a claim that failed to be released is not on its own enough to make an
offstage conversation answer a drop. Given what a mistake here costs — a file
leaving for a peer the user never picked — one mechanism is not enough.

`test/chat_drop_zone_test.dart` pins `collectDroppedFiles`'s file/folder
flagging directly, and drives `ChatDropZone`'s real `DropTarget` callback
(found in the widget tree, not simulated through the OS channel) for the
two-file, folder-only, and mixed-drop cases. It also mounts the chat offstage
exactly as go_router does — `IndexedStack` + `Offstage` + `TickerMode`, with
`skipOffstage: false` finders, since the default ones cannot see the case at
all — rather than unmounting it, which is a different scenario that passes
either way.

## Selecting messages in a conversation

A **long-press** on a bubble starts a selection with that one message; on
desktop a **secondary tap** (right-click) does the same, since a long-press
with a mouse is nobody's idiom. Once one is open a plain tap toggles, the app
bar becomes a selection bar — × , `N selected`, **Forward**, **Delete** — and
selection ends via the ×, the system back gesture, or taking the last message
back.

**Selection is one set, not a set plus a mode flag.** Every way in and out is a
change to `_selected` in `ChatScreen`, so the two cannot disagree. It is
narrowed at build time to ids still in the thread and never mutated during a
build — the shape `TransfersScreen` uses, and for the same reason: incoming
messages and ~100 staging progress ticks per share rebuild this screen
constantly, and a selection that a rebuild could drop is not a selection. A
`_selected` left naming nothing real is cleared **after** the frame, which
matters because an inbound file's id is the *sender's* `FileRef` id: a peer
reusing one a stale set still held would render that message pre-selected.

**Back leaves the selection before it leaves the screen** (a `PopScope`). A
back press that closed the conversation with a selection open throws away work
the user is in the middle of.

**While selecting there is one action path.** The in-bubble actions — Cancel,
Dismiss, and an offer's Decline/Accept/Trust — are withheld, exactly as
`TransfersScreen` withholds its per-card decisions in selection mode: those are
decisions about one file, and a live Accept sitting where the user is aiming to
toggle is the wrong tap waiting to happen.

### Delete

Confirmed first, and the confirmation promises only what can honestly be
promised beforehand: the messages go **from this device**, anything still
waiting to be sent is kept and still sent, and the other device keeps its own
copy. No counts of what will survive — what is queued can change up to the
moment the engine deletes.

The whole selection goes in **one** `chatDeleteMessages` call, and the report is
the engine's own answer rather than the request: `Deleted 3 messages`, or
`Deleted 2 messages · 1 kept because it is still being sent`. A kept row is one
whose record still backs a queued send — deleting it is what makes the drain
conclude nothing will ever settle that entry, at which point it drops the entry
and deletes the file's only staged copy — so saying why it is still on screen is
the point of the engine naming it. A refusal is reported, never swallowed: the
messages are all still there, and silence would look like it worked.

### Forward

Opens the same `showDevicePicker` sheet the Send flow uses — one device list,
not a second one. Each selected message is then sent to the chosen peer **in
thread order**, through the existing send paths: text as text, a file as a
file. No new engine call, and nothing bypasses `PeerSession`.

**A saved (by-address) pick is resolved to a real device id first.** A saved
entry's id is a locally minted timestamp the peer has never heard of, and a
conversation may only be keyed by an authenticated id — the rule Home already
applies before it will open a chat at all — so an unresolvable pick is refused
with the reason rather than filing rows into a thread every reply would miss.

**A file whose bytes are gone is excluded before anything is sent**, and named:
`invoice.pdf isn't on this device any more`. A row's `localPath` records where
its file *was* — the sender's original can move, and on Android a received
file's engine-private copy is unlinked once it is published into the user's SAF
folder — so `localFileExists` asks the filesystem up front instead of letting
the engine fail one message at a time into a row of failed bubbles in the other
thread. A selection of only-missing files sends nothing and says so.

`test/chat_selection_test.dart` covers entering/leaving, back ordering,
survival across an incoming message, the single delete call with exactly the
selected ids, the engine's `removed`/`kept` rendering, and forwarding's order,
per-kind routing and exclusion.

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
