# Stacking Selection Tray Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user stack multiple heterogeneous items (files, folders, text, clipboard) into one persistent selection, review/edit it, and send the whole batch to one device — LocalSend-style.

**Architecture:** Extend the existing `StagingStore` to also hold inline text items; reuse the staged bottom sheet as a growable "tray" with a source toolbar; add one shared `sendStaged` helper that materializes text to temp `.txt` at send time and batches the send; make Home content-first (tap a device → send the stack when non-empty) with a persistent selection bar. No Rust/FFI/engine/discovery/transport changes.

**Tech Stack:** Flutter (Dart), Material 3, `ChangeNotifier` stores, `flutter_test`.

## Global Constraints

- Flutter-only. **No** changes to Rust, FFI, engine, discovery, trust, routing, or CLI.
- Text rides the existing wire convention: temp file named `peerbeam-clipboard-<digits>.txt` (must match `messageFileName` = `^peerbeam-clipboard-\d+\.txt$`), so the receiver + History keep rendering it as a message.
- Never read file bytes into memory for staging (path + metadata only); text content is small and held inline.
- Content-first behavior: a device tap sends the stack **only when it is non-empty**; an empty stack must still open the file picker (preserve today's behavior).
- Keep the existing test suite green: `flutter analyze` clean, `flutter test` green.
- Spacing/size tokens: `AppSpace.{xxs=4,xs=8,sm=12,md=16,lg=20,xl=24,xxl=32}`, `AppIcons.{sm=18,md=22,lg=28}`, `AppMotion.fast=150ms`, `AppMotion.curve=easeOutCubic`. `formatBytes(int)` lives in `lib/state/models.dart`.
- Every commit message ends with: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- All commands run from the `flutter/` directory unless noted.

---

### Task 1: Staging model — text items

**Files:**
- Modify: `flutter/lib/state/staging.dart` (whole file rewritten below)
- Test: `flutter/test/staging_test.dart` (append tests)

**Interfaces:**
- Produces:
  - `enum StagedKind { file, folder, text }`
  - `StagedFile` with fields `String id, String path, String name, int size, StagedKind kind, String? text`; getters `bool isDirectory`, `bool isText`, `String preview`; factories `StagedFile({required String path, required String name, required int size, bool isDirectory})` and `StagedFile.text({required String id, required String content})`.
  - `StagingStore` methods: `int add(Iterable<StagedFile>)` (dedup files/folders by path; text never deduped), `StagedFile addText(String content)`, `void remove(String id)`, `void clear()`; getters `items`, `count`, `isEmpty`, `isNotEmpty`, `totalBytes`.

- [ ] **Step 1: Add failing tests** — append inside the `group('StagingStore', ...)` block in `flutter/test/staging_test.dart` (before the closing `});` of the group), and add `import 'package:peerbeam/state/staging.dart';` already present:

```dart
    test('text items are distinct, not deduped, and sized by content', () {
      final s = StagingStore();
      final a = s.addText('hello');
      final b = s.addText('hello'); // identical content is still a new item
      expect(s.count, 2);
      expect(a.id == b.id, isFalse);
      expect(a.isText, isTrue);
      expect(a.kind, StagedKind.text);
      expect(a.size, 5); // 'hello' = 5 UTF-8 bytes
    });

    test('totalBytes includes text byte length', () {
      final s = StagingStore();
      s.add([file('/a', 100)]);
      s.addText('abc'); // 3 bytes
      expect(s.totalBytes, 103);
    });

    test('remove by id removes a text item', () {
      final s = StagingStore();
      final t = s.addText('bye');
      expect(s.count, 1);
      s.remove(t.id);
      expect(s.isEmpty, isTrue);
    });

    test('preview truncates a long single line', () {
      final s = StagingStore();
      final t = s.addText('x' * 200);
      expect(t.preview.endsWith('…'), isTrue);
      expect(t.preview.length, lessThanOrEqualTo(81));
    });
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `flutter test test/staging_test.dart`
Expected: FAIL — `addText`, `isText`, `StagedKind`, `preview` are undefined.

- [ ] **Step 3: Rewrite the model** — replace the entire contents of `flutter/lib/state/staging.dart` with:

```dart
import 'dart:convert';

import 'package:flutter/foundation.dart';

/// What kind of thing is staged. Files/folders carry a real [path]; text
/// carries inline [text] (materialized to a temp file only at send time).
enum StagedKind { file, folder, text }

/// An item queued to be sent. For files/folders only path + metadata are held
/// (never bytes), so staging a 50 GB file costs nothing. For text the (small)
/// content is held inline until send.
@immutable
class StagedFile {
  /// Stable identity + removal key. For files/folders this is the [path]; for
  /// text it is a store-supplied counter id.
  final String id;
  final String path;
  final String name;
  final int size;
  final StagedKind kind;

  /// Inline content for text items; null for files/folders.
  final String? text;

  const StagedFile._({
    required this.id,
    required this.path,
    required this.name,
    required this.size,
    required this.kind,
    this.text,
  });

  /// A staged file or folder. Identity is the [path] (dedup key).
  factory StagedFile({
    required String path,
    required String name,
    required int size,
    bool isDirectory = false,
  }) => StagedFile._(
    id: path,
    path: path,
    name: name,
    size: size,
    kind: isDirectory ? StagedKind.folder : StagedKind.file,
  );

  /// A staged text message. [id] must be unique per item (the store supplies a
  /// counter-based id). Size is the UTF-8 byte length of [content].
  factory StagedFile.text({required String id, required String content}) =>
      StagedFile._(
        id: id,
        path: '',
        name: 'Text message',
        size: utf8.encode(content).length,
        kind: StagedKind.text,
        text: content,
      );

  bool get isDirectory => kind == StagedKind.folder;
  bool get isText => kind == StagedKind.text;

  /// A short single-line preview for text items ('' for files/folders).
  String get preview {
    final t = text;
    if (t == null) return '';
    final firstLine = t.trimLeft().split('\n').first;
    return firstLine.length > 80 ? '${firstLine.substring(0, 80)}…' : firstLine;
  }
}

/// Holds the set of items staged for sending. Pure and UI-agnostic: pickers /
/// drop zone / composer feed it, screens render it. Files/folders deduplicate
/// by path; text items are always distinct.
class StagingStore extends ChangeNotifier {
  final List<StagedFile> _items = [];
  int _textSeq = 0;

  List<StagedFile> get items => List.unmodifiable(_items);
  int get count => _items.length;
  bool get isEmpty => _items.isEmpty;
  bool get isNotEmpty => _items.isNotEmpty;
  int get totalBytes => _items.fold(0, (sum, f) => sum + f.size);

  /// Add files/folders, ignoring any whose path is already staged. Returns how
  /// many were newly added.
  int add(Iterable<StagedFile> files) {
    var added = 0;
    for (final f in files) {
      if (f.path.isNotEmpty && _items.any((e) => e.path == f.path)) continue;
      _items.add(f);
      added++;
    }
    if (added > 0) notifyListeners();
    return added;
  }

  /// Add a text message and return the created item (its [StagedFile.id] is the
  /// removal key).
  StagedFile addText(String content) {
    final item = StagedFile.text(id: 'text-${_textSeq++}', content: content);
    _items.add(item);
    notifyListeners();
    return item;
  }

  void remove(String id) {
    final before = _items.length;
    _items.removeWhere((f) => f.id == id);
    if (_items.length != before) notifyListeners();
  }

  void clear() {
    if (_items.isEmpty) return;
    _items.clear();
    notifyListeners();
  }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `flutter test test/staging_test.dart`
Expected: PASS (all old + new tests; old `remove('/a')` still works because a file's `id == path`).

- [ ] **Step 5: Commit**

```bash
git add flutter/lib/state/staging.dart flutter/test/staging_test.dart
git commit -m "feat(staging): text items in the selection stack

StagedKind (file/folder/text), stable id, inline text with preview, and
StagedFile.text; totalBytes counts text byte-length; remove keys off id.
Files/folders still dedup by path; text items are always distinct.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Text helpers — payload writer, composer, add-to-stack

**Files:**
- Modify: `flutter/lib/features/send/send_text.dart` (whole file rewritten below)
- Modify: `flutter/lib/main.dart` (`_onSharedText`)
- Test: `flutter/test/send_text_test.dart` (create)

**Interfaces:**
- Consumes: `StagingStore.addText` (Task 1); `showStagedFilesSheet` (existing).
- Produces:
  - `final RegExp messageFileName` (unchanged).
  - `Future<String> writeTextPayload(String text)` → temp path matching `messageFileName`.
  - `Future<String?> composeText(BuildContext, {String prefill})` → the compose dialog.
  - `Future<void> addTextToStack(BuildContext)` → compose (clipboard-prefilled) → `addText` → open tray.
  - `Future<void> addClipboardToStack(BuildContext)` → add current clipboard text as an item (snackbar if empty).
  - `Future<void> showMessageDialog(BuildContext, {required String title, required String text})` (unchanged).
  - **Removed:** `composeAndSendText`, `sendTextToDevice`.

- [ ] **Step 1: Write the failing test** — create `flutter/test/send_text_test.dart`:

```dart
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/features/send/send_text.dart';

void main() {
  test('writeTextPayload writes wire-convention file with the content', () async {
    final path = await writeTextPayload('hello there');
    final f = File(path);
    expect(await f.exists(), isTrue);
    expect(await f.readAsString(), 'hello there');
    expect(messageFileName.hasMatch(f.uri.pathSegments.last), isTrue);
  });

  test('writeTextPayload yields unique paths for back-to-back calls', () async {
    final a = await writeTextPayload('one');
    final b = await writeTextPayload('two');
    expect(a == b, isFalse);
  });
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `flutter test test/send_text_test.dart`
Expected: FAIL — `writeTextPayload` not defined.

- [ ] **Step 3: Rewrite `send_text.dart`** — replace the entire contents of `flutter/lib/features/send/send_text.dart` with:

```dart
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../app/theme.dart';
import '../../state/app_scope.dart';
import 'staged_sheet.dart';

/// The wire-name convention for a text/clipboard payload.
final RegExp messageFileName = RegExp(r'^peerbeam-clipboard-\d+\.txt$');

/// Monotonic, digits-only sequence for temp payload names — seeded from the
/// clock so temp files from different runs never collide, and guaranteed unique
/// within a run (two texts in one stack would otherwise share a millisecond).
int _payloadSeq = DateTime.now().microsecondsSinceEpoch;

/// Write [text] to a temp file using the wire convention
/// (`peerbeam-clipboard-<digits>.txt`) and return its path. The receiver and
/// History render files with this name as a message, not a downloaded file.
Future<String> writeTextPayload(String text) async {
  final file = File(
    '${Directory.systemTemp.path}/peerbeam-clipboard-${_payloadSeq++}.txt',
  );
  await file.writeAsString(text);
  return file.path;
}

/// The compose-message dialog. Returns the entered text, or null if cancelled.
Future<String?> composeText(BuildContext context, {String prefill = ''}) {
  final controller = TextEditingController(text: prefill);
  return showDialog<String>(
    context: context,
    builder: (ctx) => AlertDialog(
      title: const Text('Send text'),
      content: TextField(
        controller: controller,
        autofocus: true,
        minLines: 3,
        maxLines: 8,
        decoration: const InputDecoration(hintText: 'Type or paste a message'),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(ctx),
          child: const Text('Cancel'),
        ),
        FilledButton(
          onPressed: () => Navigator.pop(ctx, controller.text),
          child: const Text('Add'),
        ),
      ],
    ),
  );
}

/// Compose a message (prefilled from the clipboard) and add it to the stack,
/// then open the selection tray — the LocalSend-style "add text" flow.
Future<void> addTextToStack(BuildContext context) async {
  final clip = (await Clipboard.getData(Clipboard.kTextPlain))?.text ?? '';
  if (!context.mounted) return;
  final text = await composeText(context, prefill: clip);
  if (text == null || text.trim().isEmpty || !context.mounted) return;
  final staging = AppScope.of(context).staging;
  staging.addText(text);
  showStagedFilesSheet(context, staging);
}

/// Add the current clipboard text to the stack (no dialog). Snackbars if empty.
Future<void> addClipboardToStack(BuildContext context) async {
  final clip = (await Clipboard.getData(Clipboard.kTextPlain))?.text ?? '';
  if (!context.mounted) return;
  if (clip.trim().isEmpty) {
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(const SnackBar(content: Text('Clipboard is empty')));
    return;
  }
  AppScope.of(context).staging.addText(clip);
}

/// Show a text payload as a message dialog (content + Copy), like LocalSend —
/// instead of it looking like a downloaded file. [title] e.g. "Message from X".
Future<void> showMessageDialog(
  BuildContext context, {
  required String title,
  required String text,
}) {
  return showDialog<void>(
    context: context,
    builder: (ctx) {
      final scheme = Theme.of(ctx).colorScheme;
      return AlertDialog(
        title: Text(title),
        content: ConstrainedBox(
          constraints: const BoxConstraints(maxHeight: 320),
          child: SingleChildScrollView(
            child: Container(
              width: double.maxFinite,
              padding: const EdgeInsets.all(AppSpace.sm),
              decoration: BoxDecoration(
                color: scheme.surfaceContainerHighest,
                borderRadius: BorderRadius.circular(AppRadius.md),
              ),
              child: SelectableText(text),
            ),
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx),
            child: const Text('Close'),
          ),
          FilledButton.icon(
            onPressed: () {
              Clipboard.setData(ClipboardData(text: text));
              Navigator.pop(ctx);
              ScaffoldMessenger.of(ctx)
                ..hideCurrentSnackBar()
                ..showSnackBar(const SnackBar(content: Text('Copied')));
            },
            icon: const Icon(Icons.copy_rounded, size: AppIcons.sm),
            label: const Text('Copy'),
          ),
        ],
      );
    },
  );
}
```

- [ ] **Step 4: Update the shared-text intent** in `flutter/lib/main.dart`. Replace the whole `_onSharedText` method (currently lines ~111–130) with:

```dart
  /// Shared text arrived: add it to the selection stack and open the tray
  /// (same path as any other staged item).
  void _onSharedText() {
    final text = _android.sharedText.value;
    if (text == null || text.trim().isEmpty) return;
    _android.sharedText.value = null; // consume
    _state.staging.addText(text);
    _openStagedSheet();
  }
```

The existing `import 'features/send/send_text.dart';` stays (still used by `_showMessage` → `showMessageDialog`). The `staged_sheet.dart` import stays. No other change to `main.dart`.

- [ ] **Step 5: Run the affected tests**

Run: `flutter test test/send_text_test.dart`
Expected: PASS.

- [ ] **Step 6: Confirm no lingering references** to removed functions:

Run: `cd flutter && grep -rn "composeAndSendText\|sendTextToDevice" lib test`
Expected: no output (Task 4 & 5 replace the `home_screen.dart` call site; if this grep shows `home_screen.dart`, that's fixed in Task 5 — proceed, but note it).

> Note: `home_screen.dart` still references `composeAndSendText` until Task 5. `flutter analyze` will report that one error until Task 5 lands; that is expected and resolved there. Do **not** `flutter analyze` the whole project as this task's gate — the per-file tests above are the gate.

- [ ] **Step 7: Commit**

```bash
git add flutter/lib/features/send/send_text.dart flutter/lib/main.dart flutter/test/send_text_test.dart
git commit -m "feat(send): text helpers for the stack (writeTextPayload, composeText, addTextToStack)

Extract the temp-payload writer (unique digits-only names so multiple texts
in one stack don't collide) and the compose dialog; add addTextToStack /
addClipboardToStack. Shared-text intent now routes into the stack. Removes
the immediate-send composeAndSendText/sendTextToDevice (superseded by
sendStaged in the next task).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: `sendStaged` helper

**Files:**
- Create: `flutter/lib/features/send/send_staged.dart`
- Test: `flutter/test/send_staged_test.dart` (create)

**Interfaces:**
- Consumes: `StagingStore` + `StagedKind` (Task 1), `writeTextPayload` (Task 2), `AppScope.of`, `TransferRepository.send`/`sendFolder`, `friendlyError`, `PeerTarget`.
- Produces: `Future<void> sendStaged(BuildContext context, PeerTarget target, String targetName)`.

- [ ] **Step 1: Write the failing test** — create `flutter/test/send_staged_test.dart`:

```dart
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/features/send/send_staged.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/staging.dart';
import 'package:peerbeam/state/stores.dart';
import 'sdk/fake_peerbeam.dart';

void main() {
  testWidgets('sendStaged batches files + materialized text and streams folders', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    final state = AppState.live(fake);
    state.staging.add([
      StagedFile(path: '/x/a.bin', name: 'a.bin', size: 10),
      StagedFile(path: '/x/dir', name: 'dir', size: 0, isDirectory: true),
    ]);
    state.staging.addText('hello world');

    late BuildContext ctx;
    await tester.pumpWidget(
      AppScope(
        state: state,
        child: MaterialApp(
          home: Scaffold(
            body: Builder(
              builder: (c) {
                ctx = c;
                return const SizedBox();
              },
            ),
          ),
        ),
      ),
    );

    await sendStaged(
      ctx,
      const PeerTarget(name: 'Laptop', addresses: ['host'], port: 49600),
      'Laptop',
    );
    await tester.pump();

    // One batch send with the file + a materialized clipboard payload.
    final sendCall = fake.calls.firstWhere(
      (c) => c.startsWith('send:'),
      orElse: () => '',
    );
    expect(sendCall, contains('/x/a.bin'));
    expect(sendCall, contains('peerbeam-clipboard-'));
    // Folder streamed on its own.
    expect(fake.calls, contains('sendFolder:/x/dir'));
    // Stack cleared on success.
    expect(state.staging.isEmpty, isTrue);
  });
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `flutter test test/send_staged_test.dart`
Expected: FAIL — `sendStaged` / `send_staged.dart` not found.

- [ ] **Step 3: Create the helper** — write `flutter/lib/features/send/send_staged.dart`:

```dart
import 'package:flutter/material.dart';

import '../../sdk/error_text.dart';
import '../../sdk/models.dart' show PeerTarget;
import '../../state/app_scope.dart';
import '../../state/staging.dart';
import 'send_text.dart';

/// Send the entire staged selection to [target]: files + text (each text
/// materialized to a temp payload) go in one batch; folders stream one at a
/// time. Clears the stack on success; keeps it on error so the user can retry.
Future<void> sendStaged(
  BuildContext context,
  PeerTarget target,
  String targetName,
) async {
  final scope = AppScope.of(context);
  final staging = scope.staging;
  final items = staging.items;
  if (items.isEmpty) return;

  void snack(String m) => ScaffoldMessenger.of(context)
    ..hideCurrentSnackBar()
    ..showSnackBar(SnackBar(content: Text(m)));

  final filePaths = <String>[];
  final folderPaths = <String>[];
  final texts = <String>[];
  for (final item in items) {
    switch (item.kind) {
      case StagedKind.folder:
        folderPaths.add(item.path);
      case StagedKind.text:
        texts.add(item.text ?? '');
      case StagedKind.file:
        filePaths.add(item.path);
    }
  }

  try {
    for (final t in texts) {
      filePaths.add(await writeTextPayload(t));
    }
    if (filePaths.isNotEmpty) await scope.transfer.send(target, filePaths);
    for (final folder in folderPaths) {
      await scope.transfer.sendFolder(target, folder);
    }
    staging.clear();
    if (context.mounted) snack('Sending ${items.length} to $targetName');
  } catch (e) {
    if (context.mounted) snack(friendlyError(e));
  }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `flutter test test/send_staged_test.dart`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add flutter/lib/features/send/send_staged.dart flutter/test/send_staged_test.dart
git commit -m "feat(send): sendStaged — one helper to send the whole stack

Splits the stack into files + folders + text, materializes text to temp
payloads at send time, batches files+text in one send and streams folders
individually, clears on success and keeps the stack on error for retry.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Selection tray — source toolbar + text rows

**Files:**
- Modify: `flutter/lib/features/send/staged_sheet.dart` (whole file rewritten below)

**Interfaces:**
- Consumes: `StagingStore` (Task 1), `sendStaged` (Task 3), `composeText`/`addClipboardToStack` (Task 2), `pickFilesToStage`/`pickFolderToStage`/`isDesktop` (existing `desktop_files.dart`), `showDevicePicker` (existing), `formatBytes` (`state/models.dart`).
- Produces: `Future<void> showStagedFilesSheet(BuildContext, StagingStore)` (unchanged signature).

- [ ] **Step 1: Rewrite the sheet** — replace the entire contents of `flutter/lib/features/send/staged_sheet.dart` with:

```dart
import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../platform/desktop_files.dart';
import '../../state/app_scope.dart';
import '../../state/models.dart' show formatBytes;
import '../../state/staging.dart';
import '../../widgets/appear.dart';
import 'pick_device.dart';
import 'send_staged.dart';
import 'send_text.dart';

/// Show the selection tray. Lists what will be sent, with a source toolbar to
/// keep stacking (files/folder/text/clipboard), per-item removal, a total, and
/// a Send action that picks a device and sends the whole stack.
Future<void> showStagedFilesSheet(BuildContext context, StagingStore staging) {
  return showModalBottomSheet<void>(
    context: context,
    showDragHandle: true,
    isScrollControlled: true,
    builder: (context) => _StagedSheet(staging: staging),
  );
}

/// Pick a destination and send the whole stack, then close the sheet.
Future<void> _pickAndSend(BuildContext context, StagingStore staging) async {
  final picked = await showDevicePicker(context);
  if (picked == null || !context.mounted) return;
  await sendStaged(context, picked.target, picked.name);
  if (context.mounted) Navigator.pop(context);
}

Future<void> _addFiles(BuildContext context, StagingStore staging) async {
  final picked = await pickFilesToStage();
  if (picked.isNotEmpty) staging.add(picked);
}

Future<void> _addFolder(BuildContext context, StagingStore staging) async {
  final folder = await pickFolderToStage();
  if (folder != null) staging.add([folder]);
}

Future<void> _addText(BuildContext context, StagingStore staging) async {
  final text = await composeText(context);
  if (text != null && text.trim().isNotEmpty) staging.addText(text);
}

class _StagedSheet extends StatelessWidget {
  final StagingStore staging;
  const _StagedSheet({required this.staging});

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;

    return AnimatedBuilder(
      animation: staging,
      builder: (context, _) {
        final items = staging.items;
        return SafeArea(
          child: ConstrainedBox(
            constraints: BoxConstraints(
              maxHeight: MediaQuery.sizeOf(context).height * 0.7,
            ),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                Padding(
                  padding: const EdgeInsets.fromLTRB(
                    AppSpace.lg,
                    AppSpace.xxs,
                    AppSpace.sm,
                    AppSpace.xs,
                  ),
                  child: Row(
                    children: [
                      Text(
                        'Ready to send',
                        style: text.titleLarge?.copyWith(
                          fontWeight: FontWeight.w700,
                        ),
                      ),
                      const Spacer(),
                      if (items.isNotEmpty)
                        TextButton(
                          onPressed: staging.clear,
                          child: const Text('Clear'),
                        ),
                    ],
                  ),
                ),

                // Source toolbar — keep stacking heterogeneous items.
                Padding(
                  padding: const EdgeInsets.symmetric(horizontal: AppSpace.md),
                  child: Wrap(
                    spacing: AppSpace.xs,
                    runSpacing: AppSpace.xs,
                    children: [
                      _SourceButton(
                        icon: Icons.insert_drive_file_rounded,
                        label: 'Files',
                        onTap: () => _addFiles(context, staging),
                      ),
                      if (isDesktop)
                        _SourceButton(
                          icon: Icons.folder_rounded,
                          label: 'Folder',
                          onTap: () => _addFolder(context, staging),
                        ),
                      _SourceButton(
                        icon: Icons.chat_bubble_outline_rounded,
                        label: 'Text',
                        onTap: () => _addText(context, staging),
                      ),
                      _SourceButton(
                        icon: Icons.content_paste_rounded,
                        label: 'Clipboard',
                        onTap: () => addClipboardToStack(context),
                      ),
                    ],
                  ),
                ),
                const Gap(AppSpace.xs),

                if (items.isEmpty)
                  Padding(
                    padding: const EdgeInsets.all(AppSpace.xxl),
                    child: Text(
                      'Add files, a folder, or text to send.',
                      style: text.bodyMedium?.copyWith(
                        color: scheme.onSurfaceVariant,
                      ),
                    ),
                  )
                else
                  Flexible(
                    child: ListView.builder(
                      shrinkWrap: true,
                      padding: const EdgeInsets.symmetric(
                        horizontal: AppSpace.sm,
                      ),
                      itemCount: items.length,
                      itemBuilder: (context, i) {
                        final it = items[i];
                        return Appear(
                          index: i,
                          child: ListTile(
                            leading: Icon(
                              it.isText
                                  ? Icons.chat_bubble_outline_rounded
                                  : it.isDirectory
                                  ? Icons.folder_rounded
                                  : Icons.insert_drive_file_rounded,
                              color: scheme.primary,
                            ),
                            title: Text(
                              it.isText ? 'Text message' : it.name,
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                            ),
                            subtitle: Text(
                              it.isText
                                  ? it.preview
                                  : it.isDirectory
                                  ? 'Folder'
                                  : formatBytes(it.size),
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                            ),
                            trailing: IconButton(
                              icon: const Icon(Icons.close_rounded),
                              tooltip: 'Remove',
                              onPressed: () => staging.remove(it.id),
                            ),
                          ),
                        );
                      },
                    ),
                  ),
                Padding(
                  padding: const EdgeInsets.all(AppSpace.md),
                  child: Row(
                    children: [
                      Expanded(
                        child: Text(
                          items.isEmpty
                              ? ''
                              : '${items.length} ${items.length == 1 ? 'item' : 'items'} · ${formatBytes(staging.totalBytes)}',
                          style: text.bodyMedium?.copyWith(
                            color: scheme.onSurfaceVariant,
                          ),
                        ),
                      ),
                      FilledButton.icon(
                        onPressed: items.isEmpty
                            ? null
                            : () => _pickAndSend(context, staging),
                        icon: const Icon(Icons.send_rounded),
                        label: Text('Send ${items.length}'),
                      ),
                    ],
                  ),
                ),
              ],
            ),
          ),
        );
      },
    );
  }
}

/// A compact "add source" chip-style button for the tray toolbar.
class _SourceButton extends StatelessWidget {
  final IconData icon;
  final String label;
  final VoidCallback onTap;
  const _SourceButton({
    required this.icon,
    required this.label,
    required this.onTap,
  });

  @override
  Widget build(BuildContext context) {
    return FilledButton.tonalIcon(
      onPressed: onTap,
      icon: Icon(icon, size: AppIcons.sm),
      label: Text(label),
    );
  }
}
```

- [ ] **Step 2: Analyze the changed files**

Run: `cd flutter && dart analyze lib/features/send/staged_sheet.dart lib/features/send/send_staged.dart lib/features/send/send_text.dart lib/state/staging.dart`
Expected: "No issues found!" (the whole-project analyze still fails on `home_screen.dart`'s `composeAndSendText` until Task 5 — that's expected).

- [ ] **Step 3: Run the send/staging suites (regression)**

Run: `flutter test test/staging_test.dart test/send_staged_test.dart test/send_text_test.dart test/drop_zone_test.dart`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add flutter/lib/features/send/staged_sheet.dart
git commit -m "feat(send): selection tray — source toolbar + text rows

The staged sheet gains an add-source toolbar (Files / Folder [desktop] /
Text / Clipboard) so the stack grows in place, renders text items with a
preview, keys removal off item id, and sends via sendStaged.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Home — content-first taps + persistent selection bar

**Files:**
- Modify: `flutter/lib/features/home/home_screen.dart`
- Test: `flutter/test/home_selection_test.dart` (create)

**Interfaces:**
- Consumes: `sendStaged` (Task 3), `addTextToStack` (Task 2), `showStagedFilesSheet` + `showDevicePicker` (existing), `StagingStore` (Task 1), `formatBytes` (`state/models.dart`).
- Produces: content-first behavior in `_sendTo` / `_sendToSaved` / `_sendToAddress`; a `_SelectionBar` shown via the Home `Scaffold.bottomSheet`.

- [ ] **Step 1: Write the failing test** — create `flutter/test/home_selection_test.dart`:

```dart
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/features/home/home_screen.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/staging.dart';
import 'package:peerbeam/state/stores.dart';
import 'sdk/fake_peerbeam.dart';

void main() {
  testWidgets('persistent selection bar appears when the stack is non-empty', (
    tester,
  ) async {
    final state = AppState.live(FakePeerBeam());
    await tester.pumpWidget(
      AppScope(state: state, child: const MaterialApp(home: HomeScreen())),
    );
    await tester.pump();

    // Empty stack → no bar.
    expect(find.textContaining('item'), findsNothing);

    state.staging.add([
      StagedFile(path: '/x/a.bin', name: 'a.bin', size: 5),
    ]);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 200)); // AnimatedSize

    // Non-empty stack → the bar shows the count.
    expect(find.textContaining('1 item'), findsOneWidget);
  });
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `flutter test test/home_selection_test.dart`
Expected: FAIL — no selection bar text is rendered yet.

- [ ] **Step 3: Update imports** in `flutter/lib/features/home/home_screen.dart`. The file already imports `../send/send_text.dart`, `../send/staged_sheet.dart`, `../../state/models.dart`, `../../platform/desktop_files.dart`, `../../state/app_scope.dart`. Add these two imports alongside the existing `../send/...` imports:

```dart
import '../send/pick_device.dart';
import '../send/send_staged.dart';
```

- [ ] **Step 4: Make `_sendTo` content-first.** Replace the whole `_sendTo` method with:

```dart
  /// Send to a discovered device. Content-first: if the stack has items, send
  /// the whole stack; otherwise pick files and send those.
  Future<void> _sendTo(BuildContext context, Device device) async {
    final scope = AppScope.of(context);
    final target = scope.device.peerTarget(device.id);
    void snack(String m) => ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(m)));
    if (target == null) {
      snack('${device.name} is not reachable right now');
      return;
    }
    if (scope.staging.isNotEmpty) {
      await sendStaged(context, target, device.name);
      return;
    }
    final picked = await pickFilesToStage();
    if (picked.isEmpty || !context.mounted) return;
    try {
      await scope.transfer.send(target, picked.map((f) => f.path).toList());
      if (context.mounted) snack('Sending ${picked.length} to ${device.name}');
    } catch (e) {
      if (context.mounted) snack(friendlyError(e));
    }
  }
```

- [ ] **Step 5: Make `_sendToSaved` content-first.** Replace the whole `_sendToSaved` method with:

```dart
  /// Send to a saved device. Content-first (send the stack if non-empty).
  Future<void> _sendToSaved(BuildContext context, SavedDevice d) async {
    final scope = AppScope.of(context);
    void snack(String m) => ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(m)));
    final target = PeerTarget(name: d.name, addresses: [d.host], port: d.port);
    if (scope.staging.isNotEmpty) {
      await sendStaged(context, target, d.name);
      return;
    }
    final picked = await pickFilesToStage();
    if (picked.isEmpty || !context.mounted) return;
    try {
      await scope.transfer.send(target, picked.map((f) => f.path).toList());
      if (context.mounted) snack('Sending ${picked.length} to ${d.name}');
    } catch (e) {
      if (context.mounted) snack(friendlyError(e));
    }
  }
```

- [ ] **Step 6: Make `_sendToAddress` content-first.** Replace the whole `_sendToAddress` method with:

```dart
  /// Send to a manually-entered address (host/IP or MagicDNS name + port).
  /// Content-first: send the stack if non-empty, else pick files.
  Future<void> _sendToAddress(BuildContext context) async {
    final scope = AppScope.of(context);
    final target = await _promptForAddress(context);
    if (target == null || !context.mounted) return;
    void snack(String m) => ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(m)));
    if (scope.staging.isNotEmpty) {
      await sendStaged(context, target, target.name);
      return;
    }
    final picked = await pickFilesToStage();
    if (picked.isEmpty || !context.mounted) return;
    try {
      await scope.transfer.send(target, picked.map((f) => f.path).toList());
      if (context.mounted) snack('Sending ${picked.length} to ${target.name}');
    } catch (e) {
      if (context.mounted) snack(friendlyError(e));
    }
  }
```

- [ ] **Step 7: Re-point the "Send text" action.** In the actions Row, change the `composeAndSendText(context)` call (currently the `onPressed` of the "Send text" `FilledButton.tonalIcon`) to:

```dart
                                    onPressed: () => addTextToStack(context),
```

- [ ] **Step 8: Add the persistent bar to the Home Scaffold.** In `build`, the top-level `return Scaffold(` gains a `bottomSheet:`. Change the opening of the Home scaffold from:

```dart
    return Scaffold(
      body: SafeArea(
```

to:

```dart
    return Scaffold(
      bottomSheet: _SelectionBar(
        staging: state.staging,
        onOpen: () => showStagedFilesSheet(context, state.staging),
        onSend: () => _pickAndSendFromBar(context),
      ),
      body: SafeArea(
```

- [ ] **Step 9: Add the bar helper + widget.** Add this method inside the `HomeScreen` class (next to the other `_send*` methods):

```dart
  /// Pick a device from the persistent bar and send the current stack.
  Future<void> _pickAndSendFromBar(BuildContext context) async {
    final picked = await showDevicePicker(context);
    if (picked == null || !context.mounted) return;
    await sendStaged(context, picked.target, picked.name);
  }
```

And add this widget at the end of the file (after `_DeviceSearchDelegate`):

```dart
/// Slim bar pinned to the bottom of Home while the selection stack is
/// non-empty: item count + total, tap to open the tray, Send to pick a device.
class _SelectionBar extends StatelessWidget {
  final StagingStore staging;
  final VoidCallback onOpen;
  final VoidCallback onSend;
  const _SelectionBar({
    required this.staging,
    required this.onOpen,
    required this.onSend,
  });

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    return AnimatedBuilder(
      animation: staging,
      builder: (context, _) {
        final n = staging.count;
        return AnimatedSize(
          duration: AppMotion.fast,
          curve: AppMotion.curve,
          child: n == 0
              ? const SizedBox(width: double.infinity)
              : Material(
                  color: scheme.surfaceContainerHigh,
                  child: SafeArea(
                    top: false,
                    child: InkWell(
                      onTap: onOpen,
                      child: Padding(
                        padding: const EdgeInsets.fromLTRB(
                          AppSpace.md,
                          AppSpace.sm,
                          AppSpace.sm,
                          AppSpace.sm,
                        ),
                        child: Row(
                          children: [
                            Icon(Icons.layers_rounded, color: scheme.primary),
                            const Gap(AppSpace.sm),
                            Expanded(
                              child: Text(
                                '$n ${n == 1 ? 'item' : 'items'} · ${formatBytes(staging.totalBytes)}',
                                style: text.titleSmall,
                              ),
                            ),
                            FilledButton.icon(
                              onPressed: onSend,
                              icon: const Icon(
                                Icons.send_rounded,
                                size: AppIcons.sm,
                              ),
                              label: const Text('Send'),
                            ),
                          ],
                        ),
                      ),
                    ),
                  ),
                ),
        );
      },
    );
  }
}
```

The imports for `StagingStore` (`../../state/staging.dart`), `PeerTarget` (`../../sdk/models.dart`), and `formatBytes` (`../../state/models.dart`) are already present in `home_screen.dart`. Confirm `import '../../state/staging.dart';` exists; if not, add it.

- [ ] **Step 10: Run test to verify it passes**

Run: `flutter test test/home_selection_test.dart`
Expected: PASS.

- [ ] **Step 11: Full analyze + full test suite (the whole-project gate is valid now)**

Run: `cd flutter && flutter analyze && flutter test`
Expected: "No issues found!" and all tests PASS.

- [ ] **Step 12: Commit**

```bash
git add flutter/lib/features/home/home_screen.dart flutter/test/home_selection_test.dart
git commit -m "feat(home): content-first device taps + persistent selection bar

Tapping a nearby/saved/by-address device now sends the current selection
when the stack is non-empty (empty → pick files, as before). A slim bar
pinned to Home shows the item count + total with a one-tap Send. Send text
now adds to the stack.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Docs

**Files:**
- Modify: `docs/FEATURE_ROADMAP.md` (Done section)
- Modify: `CHANGELOG.md` (`[Unreleased]`)

- [ ] **Step 1: Add the roadmap "Done" entry.** In `docs/FEATURE_ROADMAP.md`, under `## ✅ Done since this roadmap`, add a new bullet directly after the "Send text / quick message" bullet block (before the `---` that closes the section):

```markdown
- **Stacking selection** *(LocalSend-style)* — build one selection from files,
  folders, text, and clipboard, review/remove items, then send the whole batch
  to one device. Tapping a device sends the current selection (empty selection
  → pick files); a persistent Home bar shows the count + total with one-tap Send.
```

- [ ] **Step 2: Add the CHANGELOG entry.** In `CHANGELOG.md`, replace:

```markdown
## [Unreleased]

### Changed
```

with:

```markdown
## [Unreleased]

### Added
- **Stacking selection** (LocalSend-style): build one selection from files,
  folders, text, and clipboard, review/edit it, then send the whole batch to a
  device in one go. Tapping a device now sends the current selection (empty
  selection → pick files, as before). A persistent bar on Home shows the count
  + total with a one-tap Send.

### Changed
```

- [ ] **Step 3: Commit**

```bash
git add docs/FEATURE_ROADMAP.md CHANGELOG.md
git commit -m "docs: record stacking selection (roadmap Done + changelog)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage:**
- Data model (StagedKind, id, inline text, factory, remove(id), totalBytes) → Task 1. ✓
- Text modeling Approach B (inline, materialize at send) → Task 2 (`writeTextPayload`) + Task 3 (`sendStaged`). ✓
- Selection tray toolbar (Add files/folder[desktop]/text/clipboard) + text rows → Task 4. ✓
- Shared `sendStaged` helper → Task 3. ✓
- Content-first device tap (`_sendTo`/`_sendToSaved`/`_sendToAddress`; search reuses `_sendTo`) → Task 5. ✓
- Persistent selection bar → Task 5. ✓
- "Send text" re-pointed to add-to-stack → Task 5 Step 7 (+ Task 2 `addTextToStack`). ✓
- Shared-text intent into stack → Task 2 Step 4. ✓
- Tests (staging, sendStaged, tray/home) → Tasks 1,3,5 (+ Task 2 payload). ✓
- Docs (roadmap Done + changelog) → Task 6. ✓
- Dedup: files/folders by path, text always distinct → Task 1 `add`/`addText`. ✓

**Placeholder scan:** none — every code step contains full code; every command has expected output.

**Type consistency:** `StagedFile.text({id, content})`, `StagingStore.addText(String)→StagedFile`, `remove(String id)`, `sendStaged(BuildContext, PeerTarget, String)`, `writeTextPayload(String)→Future<String>`, `composeText(BuildContext,{String prefill})→Future<String?>`, `addTextToStack(BuildContext)`, `addClipboardToStack(BuildContext)` — used identically across Tasks 1–5. `StagedKind` switch is exhaustive (file/folder/text). ✓

**Known intermediate state:** after Task 2, `home_screen.dart` still calls the removed `composeAndSendText`, so a whole-project `flutter analyze` fails until Task 5. Tasks 2–4 gate on per-file `dart analyze` + targeted tests (noted in Task 2 Step 6 and Task 4 Step 2); Task 5 Step 11 restores the full-project gate. This is deliberate to keep tasks bite-sized.
