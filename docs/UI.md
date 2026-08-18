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
  features/{home,chats,devices,transfers,history,settings}/
```

## Devices

A dashboard of every discovered device, with whatever status each one chose to
share: battery and charging, free storage, network kind, app version, and how
long ago the reading arrived (measured from **our** receipt, never from the
peer's clock — peer clocks are not synchronised).

Sharing is opt-in in Settings and **off by default**, and a device's status is
only ever sent to peers it trusts. A device that shares nothing still appears,
showing its identity and reachability under a plain "Status not shared" — it is
not an error state, and it is the honest default for any peer that has not
opted in or is running an older build.

The card renders only the fields that actually arrived. Absence is not zero: a
desktop has no battery at all, and the Windows/macOS battery collector is
deliberately unimplemented, so a card that filled those in as `0%` would be
inventing a dead battery. `test/presence_test.dart` pins this from both sides —
a missing reading must not render as zero, and a genuine `0%` must not be
swallowed as missing.

## Clipboard sync

Opt-in in Settings and **off by default**. While it is on, anything copied on a
desktop is pushed to that device's **trusted** peers; the trusted-only half is
not configurable. A received clip is written to the local clipboard and
announced with a snackbar naming the sending device — *"Clipboard from Bob"* —
because a clipboard changing underneath someone is not a thing they should have
to discover by pasting. The toast never repeats the clip: it is on their
clipboard already, and a toast is a poor place for something that may be a
password.

**The toggle admits that everything copied is sent, passwords included.** There
is no password detection in PeerBeam and there is deliberately not going to be
— see `docs/SECURITY.md`. That sentence is the whole of what a user has to
decide with, so `test/clipboard_sync_test.dart` pins it verbatim; a change that
softens it should be treated as a security regression, not a copy-edit.

**Desktop sends, every platform receives.** Android 10+ forbids reading the
clipboard from the background, so a phone can never auto-send. The watcher
refuses to start off desktop — the setting cannot turn on what the platform
forbids — and the Android build shows a note saying so, rather than leaving a
toggle that mysteriously only works one way.

The watcher polls once a second and pushes only what **changed**, and the echo
guard is the load-bearing part: a clip that arrived from a peer is accounted
for *before* it is written to the clipboard, so the next poll does not mistake
it for a local copy and send it back. Without that, two devices ping-pong a
single copy forever. Whatever is already on the clipboard when the toggle is
flipped is adopted without being sent — that is not something the user just
copied, and may well be a password they pasted earlier. A clip over the 64 KiB
wire cap is skipped, not truncated, and the user is told once.

## Auto-save rules

A Settings section — *Auto-save rules* — that chooses **where** a received file
is saved. It never chooses **whether** it is accepted, and the section says so
in its first two sentences, because a list of match criteria sitting one card
below *Auto-accept trusted devices* would otherwise read as an acceptance
filter. `test/save_rules_test.dart` pins that copy for the same reason
`clipboard_sync_test.dart` pins the clipboard warning.

```
AUTO-SAVE RULES
┌────────────────────────────────────────────────────────────┐
│ Rules choose where a file is saved. They never decide       │
│ whether it is accepted. The first rule that matches wins,   │
│ so drag to reorder; anything matching none goes to          │
│ /home/me/Downloads.                                         │
│ ⠿  *.pdf                        /srv/papers            🗑   │
│ ⠿  From pb-f4e4d56fce98         /mnt/big               🗑   │
│ ⠿  Everything                   /srv/inbox             🗑   │
│ + Add rule                                                  │
└────────────────────────────────────────────────────────────┘
```

It is a `ReorderableListView` because **the order is the tie-break**: the first
rule that matches a file chooses its directory, and dragging is the only way to
change which of two overlapping rules applies. There is deliberately no
specificity score — a list a user can see and reorder is a list whose outcome
they can predict. A rule with no criteria reads as *"Everything"* rather than
as a blank row, and any rule stranded beneath one is marked *"Never reached"*
beside itself, since the alternative is adding a rule and quietly wondering why
nothing ever goes there.

The destination comes from the **native folder picker**, not a text field: it
must be an absolute path whose parent exists, and typing one is how you produce
the error the engine then has to refuse. The device criterion is a dropdown
over known devices for a sharper reason — what is stored is the authenticated
device id, and a free-text name is something any peer can claim.

Edits go through `pb_rules_set`, which **validates and can refuse**, so the
list on screen is adopted only once the engine has stored it. An optimistic
update would leave someone looking at a rule that does not exist while
believing their files were being sorted; a refusal instead surfaces the
engine's own message, which names the offending rule and what is wrong with its
path.

**Desktop and headless only.** Android receives into the SAF folder the user
granted and cannot write to any other location, so there is nowhere for a rule
to send anything. The section is replaced there by a plain statement of why —
driven by the engine's `rules_supported`, not by a platform check in Dart,
because the engine is what knows and a second opinion could disagree with it.
An engine that predates the flag reads as unsupported: offering an editor that
cannot work is the failure that default exists to prevent. (I12 — the
limitation is documented, not designed around.)

A file matching no rule goes to the save directory, exactly as every file did
before this existed. A user who never opens this section sees no change at all.

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

## A device that has never connected before

An approval prompt for a device this one has never spoken to no longer looks
like an approval prompt for a laptop used daily. When the transfer is first
contact, the prompt gains a panel — **This device has never connected before**
— and shows the session's **pairing code**: a 128-bit safety number both
devices derive from the keys they actually negotiated, in the engine's own
grouping of eight groups of four uppercase hex digits.

**The code is shown, never checked here.** This device cannot know what the
other screen displays; the comparison is the user's, out of band, and the copy
says to look at the other device *itself* rather than at a message or
screenshot of it — an attacker able to relay a handshake can usually relay a
screenshot too. Nothing in the UI may imply PeerBeam verified anything.

It is shown **in full**. All 128 bits are what make the number expensive to
forge, so there is no ellipsis, no `maxLines` and no "tap to see the rest"; it
wraps instead, and is selectable for reading aloud or comparing character by
character.

The panel appears whenever a device is new — **not** only when the
confirmation setting is on. Knowing a device has never connected before, and
being able to check it, is worth having by default; the setting decides whether
that check is *required*.

Both places a file can be accepted carry it: the Transfers card, and a file
offered inside a conversation. Routing the chat one around the check on the
grounds that a conversation implies familiarity would leave the gate guarding
one of the two ways a file gets accepted.

### Requiring the check

**Settings → Transfers → Verify new devices with a pairing code**
(`require_pairing_confirmation`, **off by default** — the same setting the CLI
reads). With it on, tapping **Accept** or **Trust** on a first-contact transfer
opens a dialog showing the code, and only its **The codes match** action
accepts anything.

- **Confirmation is a decision, not a formality.** Nothing is pre-selected,
  there is no default action, and dismissing the dialog — Cancel, a back
  gesture, a tap outside — counts as *not* confirmed. The engine agrees: only a
  literal `true` on the accept payload satisfies it.
- **Cancelling costs nothing.** It accepts nothing *and* declines nothing. The
  transfer stays waiting, so the user can go and read the other screen and come
  back. Being asked to verify a device must never cost them the file.
- **Decline is never gated.** Refusing needs no verification, and it is exactly
  the answer a user who cannot match the codes needs to be able to give without
  another dialog in the way. Declining a first-contact transfer also **un-pins**
  the peer; see [SECURITY.md](SECURITY.md#a-refused-first-contact-un-pins-other-endings-do-not).
- **Trust is gated too.** It grants standing auto-accept, so letting it skip a
  check the weaker Accept honoured would guard only the lesser act.
- **A batch cannot confirm.** With the check on, a first-contact transfer is
  never handed to **Accept all** or a selection: one tap answers for every card
  on screen, and comparing a safety number is a per-device act nobody performed
  there. It is reported as failed rather than as "no longer waiting" — it very
  much still is — and can be accepted from its own card.

The copy in the panel, the dialog and the setting is pinned verbatim by
`test/pairing_test.dart`, for the same reason the clipboard warning is: a
softened rewrite would be a security regression dressed as a copy-edit.

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

## Interrupted transfers

A transfer that ends because the link dropped or the app was closed mid-flight
is **interrupted**, not failed. The engine keeps a checkpoint for it, and the
Transfers screen shows it as a row in its own state.

**It survives a restart.** `TransferState.interrupted` is the only transfer
state that outlives the process: it is rebuilt from the engine's checkpoint by
`TransferRepository.refreshInterrupted()`, called from `main.dart` after
`initialize()` — the same "fetch after init" rule `HistoryRepository` documents,
because an engine call any earlier only answers `not_initialised` and being
swallowed would leave a cold start looking as though nothing had been
interrupted. Mid-session, the engine's `transfer_interrupted` event arrives
*after* the transfer's own terminal event and puts the row back.

**The row says how far it got.** `320 MB / 4.0 GB`, with the bar where the
transfer actually stopped. A resumable transfer that showed no progress would
give the user no reason to resume rather than start again.

**Resume and Discard, not Pause and Cancel.** There is nothing to pause and
nothing to cancel — the transfer is already over. Discard exists because
otherwise the row is clutter nothing can ever clear: no event will complete it
and no retry will remove it. Discarding also reclaims the partial file the
engine was holding for it.

**Resume here is a different verb from the Resume on a paused transfer.** A
paused transfer un-pauses (`pb_transfer_resume`); an interrupted one is
re-dialed from its checkpoint (`pb_transfer_resume_interrupted`). The repository
keeps them as two methods for that reason, and the test asserts the button calls
the second and never the first — calling the wrong one on a dead transfer would
silently do nothing.

**An incoming transfer shows no Resume.** The transfer protocol is
sender-driven, so this side cannot ask a peer to start sending again; the card
says **"Waiting for sender"** and offers only Discard. Showing a Resume that
could not work would be worse than showing none, so the button is driven by the
engine's own `resumable` flag rather than by anything the UI infers.

**A refusal is shown.** Unlike pause/resume/cancel, `resumeInterrupted` reaches
the error channel on failure: it can legitimately refuse — the peer may be
unreachable, or the source file may have changed since the transfer stopped, in
which case resuming would append the wrong bytes to the receiver's partial file.
Swallowing that would leave the user tapping a button that does nothing, which
is the fail-open shape this project has already fixed twice.

**It is not counted as work.** Interrupted rows are excluded from `activeCount`,
so the nav badge does not sit permanently lit, and from `awaitingApproval`, so
they can never be swept into a bulk Accept — nobody is being asked anything.

`test/transfers_screen_test.dart` pins the action pair, the progress line, the
two-verbs distinction, the inbound case and the badge exclusion;
`test/data/repository_test.dart` pins the same at the repository seam, plus that
a live transfer wins over a checkpoint bearing its id.

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
