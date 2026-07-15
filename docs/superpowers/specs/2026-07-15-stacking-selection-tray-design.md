# Stacking selection tray (LocalSend-style) — design

**Date:** 2026-07-15
**Status:** Approved (design); pending implementation plan
**Area:** Flutter send flow

## Goal

Bring LocalSend's "stacking" model to PeerBeam's main send flow: add multiple
heterogeneous items (files, folders, text, clipboard) into one persistent
**selection**, review/edit it, then send the whole batch to one chosen device.

## Current state (audit)

PeerBeam already stacks **files + folders**, but the model is incomplete:

- `StagingStore` (`lib/state/staging.dart`) — holds `StagedFile` (path + name +
  size + `isDirectory`; never bytes), dedup by path, `add` / `remove(path)` /
  `clear` / `count` / `totalBytes`.
- `showStagedFilesSheet` (`lib/features/send/staged_sheet.dart`) — bottom sheet:
  lists items, per-item remove, Clear, total, **Send N** → device picker →
  `transfer.send(target, files)` + per-folder `transfer.sendFolder` → `clear`.
- Feeders: `_pickFiles` / `_pickFolder` (home), desktop `DropZone`, Android
  share-in (`_openStagedSheet`).

**Gaps vs LocalSend:**

1. The sheet is terminal — no "add more" affordance once open.
2. Text/clipboard are **not** stackable. `composeAndSendText` is a separate
   immediate-send flow (temp `.txt` → device pick → send) that bypasses the stack.
3. Device-first taps ignore the stack — `_sendTo` / `_sendToSaved` / search /
   `_sendToAddress` always re-pick files fresh, never send what's staged.

**Downstream already supports it:** `transfer.send(peer, List<String> paths)`
batches; text rides as a temp `peerbeam-clipboard-*.txt`, which the receiver and
History already render as a message (`messageFileName` regex).

## Decisions (locked with owner)

- **Content-first device tap:** if the stack is non-empty, tapping any device
  (nearby / saved / search / by-address) sends the whole stack. Empty stack →
  keep today's pick-then-send behavior.
- **Sources:** files, folders, text, **and** clipboard all stack together.
- **Presentation:** reuse the existing staged bottom sheet (add a source
  toolbar inside it) + a persistent selection bar on Home. No new screen/route.
- **Text modeling — Approach B:** hold text **inline** in the stack (string +
  preview); materialize a temp `peerbeam-clipboard-<ts>.txt` only at send time.
  Keeps the store byte-free until needed, no orphan-temp-file cleanup, and lands
  on the identical wire convention. (Approach A — materialize at add-time —
  rejected for its temp-file lifecycle burden.)

## Components

### 1. Data model — `lib/state/staging.dart`

- Add `enum StagedKind { file, folder, text }`.
- `StagedFile` gains:
  - `String id` — stable identity + removal key. For files/folders `id == path`;
    for text, a monotonic per-store counter (`text-<n>`).
  - `StagedKind kind`.
  - `String? text` — inline content for text items (null otherwise).
  - `String get preview` — first line / truncated content for display.
- Keep the existing `StagedFile({path, name, size, isDirectory})` constructor,
  deriving `kind` from `isDirectory`, so `DropZone` and `desktop_files` need no
  change.
- Add `StagedFile.text(String content)` factory: `path` empty, `name` a short
  label (e.g. `"Text message"`), `size` = UTF-8 byte length, `kind = text`.
- `isDirectory` kept as a `=> kind == StagedKind.folder` getter (compat).
- Dedup: files/folders by `path` (unchanged); text items are **never** deduped
  (each add is distinct).
- `remove(String path)` → `remove(String id)`; callers pass `item.id`.
- `totalBytes` counts text byte-length in addition to file sizes.

### 2. Selection tray — `lib/features/send/staged_sheet.dart`

- Source toolbar at top of the sheet:
  - **Add files** → `pickFilesToStage` → `staging.add`.
  - **Add folder** → `pickFolderToStage` → `staging.add`. **Desktop only**
    (parity with current Home gating).
  - **Add text** → compose dialog → `StagedFile.text` → `staging.add`.
  - **Paste clipboard** → read `Clipboard.kTextPlain`; empty → snackbar
    "Clipboard is empty"; else add as a text item.
  - Adding does **not** close the sheet.
- Item rows: text items render with a chat icon + `preview` + remove; files /
  folders unchanged.
- **Send N** routes through the shared `sendStaged` helper (below).

### 3. Shared send helper (new small unit)

`Future<void> sendStaged(BuildContext, StagingStore, PeerTarget, String name)`:

1. Split items → file paths, folder paths, text items.
2. Materialize each text item → temp `peerbeam-clipboard-<ts>.txt`
   (`Directory.systemTemp`), collecting the paths.
3. `transfer.send(target, filePaths + materializedTextPaths)` (single batch);
   then per-folder `transfer.sendFolder(target, folderPath)`.
4. `staging.clear()`; snackbar `Sending <N> to <name>`.
5. On error → `friendlyError` snackbar; stack **not** cleared (user can retry).

Replaces the duplicated pick-and-send blocks in Home and the temp-file logic in
`sendTextToDevice`. Home ID: exact file location decided in the plan (either a
new `lib/features/send/send_staged.dart` or alongside the sheet).

### 4. Home — content-first + persistent bar — `lib/features/home/home_screen.dart`

- `_sendTo` / `_sendToSaved` / search result / `_sendToAddress`: **if
  `staging.isNotEmpty` → `sendStaged` to that device; else pick-then-send** (the
  existing behavior, preserved).
- **Persistent selection bar** shown when `staging.count > 0`: slim animated bar
  (`N items · <size>` + a **Send** button). Tap the bar → reopen the tray; Send
  → device picker → `sendStaged`. Implemented on Home's `Scaffold` (`bottomSheet`
  slot or a `Positioned` overlay — exact widget chosen in the plan). Listens to
  `staging` via `AnimatedBuilder`.
- "Send text" secondary action re-pointed to **add-to-stack** (compose → text
  item → open tray) instead of immediate send. "Send files" / folder / drop
  already stage.

### 5. Out of scope / unchanged

- Android shared-text intent (`_onSharedText`) may optionally route into the
  stack for consistency; not required for this feature — leave as-is unless
  trivial.
- Transfer engine, FFI, Rust, discovery, trust, routing: **no changes.**
- CLI: no changes (wire convention unchanged).

## Data flow

```
add (files/folder/text/clipboard)
      → StagingStore.add(...)  (dedup files/folders by path; text always new)
      → tray + persistent bar reflect count/total (ChangeNotifier)

send (tray Send | bar Send | device tap when stack non-empty)
      → pick target (device picker) OR the tapped device
      → sendStaged(): materialize texts → transfer.send(batch) + sendFolder(each)
      → staging.clear()
```

## Error handling

- Send failure → `friendlyError` snackbar; stack preserved for retry.
- Unreadable/removed staged file at send time → surfaced by the engine's error
  stream (existing snackbar path); other items in the batch are unaffected per
  current `send` semantics.
- Empty clipboard on Paste → snackbar, no item added.
- Empty text (whitespace only) → not added.

## Testing

- **`StagingStore` unit** (`test/`): file/folder dedup by path; text items
  distinct + counter ids; `remove(id)`; `clear`; `totalBytes` includes text
  byte-length; `isDirectory` compat getter.
- **`sendStaged` unit/widget:** mixed stack (files + folder + text) → fake
  `PeerBeamApi` receives `send` with all file + materialized-text paths and
  `sendFolder` per folder; stack cleared on success, kept on error.
- **Tray widget:** source toolbar present; Add folder hidden off-desktop; Send
  disabled when empty; text row shows preview.
- **Content-first:** with a non-empty stack, a device tap calls `sendStaged`
  (not the file picker).
- Existing suite stays green (`flutter analyze` clean, `flutter test` green).

## Documentation

- `docs/FEATURE_ROADMAP.md` → move "stacking / multi-item selection" into the
  "Done since this roadmap" section.
- `CHANGELOG.md` → `[Unreleased]` entry.
- This spec committed under `docs/superpowers/specs/`.

## Risks

- Content-first is a visible behavior change; empty-stack device tap must still
  open the file picker (explicitly preserved).
- Persistent bar must pin above the shell's nav bar/rail; verify on compact and
  wide (rail) layouts.
- `remove(path)` → `remove(id)` signature change: update all call sites.
