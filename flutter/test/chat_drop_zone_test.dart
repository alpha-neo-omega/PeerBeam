// Chat-screen drag & drop: dropping files onto an open conversation sends
// them to that peer, the same as attach — and a dropped folder is refused
// with a message naming it, since a chat file message carries exactly one
// file and the engine's `prepare_file_send` rejects a directory outright.
//
// The widget-level tests below exercise the REAL wiring: the `DropTarget`
// `ChatDropZone` builds is located in the tree and its actual `onDragDone`
// closure is invoked directly with a constructed `DropDoneDetails` — the same
// production callback a real native drop would call — rather than simulating
// the OS-level drag/geometry plumbing `desktop_drop` drives that callback
// through, which depends on hit-testing and platform-specific quirks that
// have nothing to do with this app's own logic.

import 'dart:io';

import 'package:desktop_drop/desktop_drop.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/chat/chat_screen.dart';
import 'package:peerbeam/features/send/drop_zone.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

const _peer = PeerTarget(
  id: 'pb-bob',
  name: 'Bob',
  addresses: ['127.0.0.1'],
  port: 49600,
);

/// A real file on disk with [contents] — `collectDroppedFiles` genuinely
/// calls `FileSystemEntity.isDirectorySync` and `File.length()`, so a
/// constructed path with nothing behind it would not exercise the real
/// folder-detection/size-read logic under test.
File _tempFile(Directory dir, String name, String contents) {
  final f = File('${dir.path}/$name')..writeAsStringSync(contents);
  return f;
}

void main() {
  late Directory tmp;

  setUp(() => tmp = Directory.systemTemp.createTempSync('chat_drop_test_'));
  tearDown(() => tmp.deleteSync(recursive: true));

  group('collectDroppedFiles', () {
    test('files are staged with their real size, never read', () async {
      final a = _tempFile(tmp, 'a.bin', 'aaaa'); // 4 bytes
      final b = _tempFile(tmp, 'b.bin', 'bb'); // 2 bytes

      final staged = await collectDroppedFiles(
        DropDoneDetails(
          files: [DropItemFile(a.path), DropItemFile(b.path)],
          localPosition: Offset.zero,
          globalPosition: Offset.zero,
        ),
      );

      expect(staged, hasLength(2));
      expect(staged[0].path, a.path);
      expect(staged[0].size, 4);
      expect(staged[0].isDirectory, isFalse);
      expect(staged[1].path, b.path);
      expect(staged[1].size, 2);
      expect(staged[1].isDirectory, isFalse);
    });

    test('a directory is flagged, not read for a size', () async {
      final folder = Directory('${tmp.path}/myfolder')..createSync();

      final staged = await collectDroppedFiles(
        DropDoneDetails(
          files: [DropItemFile(folder.path)],
          localPosition: Offset.zero,
          globalPosition: Offset.zero,
        ),
      );

      expect(staged, hasLength(1));
      expect(staged.single.isDirectory, isTrue);
      expect(staged.single.size, 0);
    });

    test('a mixed drop flags each item correctly, in order', () async {
      final file = _tempFile(tmp, 'a.bin', 'x');
      final folder = Directory('${tmp.path}/myfolder')..createSync();

      final staged = await collectDroppedFiles(
        DropDoneDetails(
          files: [DropItemFile(file.path), DropItemFile(folder.path)],
          localPosition: Offset.zero,
          globalPosition: Offset.zero,
        ),
      );

      expect(staged, hasLength(2));
      expect(staged[0].isDirectory, isFalse);
      expect(staged[1].isDirectory, isTrue);
    });
  });

  group('ChatDropZone (wired through ChatScreen)', () {
    Widget screen(AppState state) => AppScope(
      state: state,
      child: const MaterialApp(
        home: ChatScreen(peerId: 'pb-bob', peer: _peer),
      ),
    );

    Future<AppState> open(WidgetTester tester, FakePeerBeam fake) async {
      final state = AppState.live(fake);
      addTearDown(state.dispose);
      await tester.pumpWidget(screen(state));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));
      return state;
    }

    /// Invoke the real `DropTarget.onDragDone` found in the tree — the exact
    /// closure `ChatDropZone` wires up in production — with a drop of
    /// [paths], then pump long enough for the fire-and-forget
    /// `chat.sendFile` calls and any snackbar to land.
    ///
    /// Wrapped in [WidgetTester.runAsync]: `collectDroppedFiles` does a real
    /// `File.length()` read per file, and a `testWidgets` body otherwise runs
    /// in a FakeAsync zone where a genuine `dart:io` future never resolves —
    /// exactly the case `runAsync` exists to escape.
    Future<void> drop(WidgetTester tester, List<String> paths) async {
      final target = tester.widget<DropTarget>(find.byType(DropTarget));
      await tester.runAsync(() async {
        target.onDragDone!(
          DropDoneDetails(
            files: paths.map(DropItemFile.new).toList(),
            localPosition: Offset.zero,
            globalPosition: Offset.zero,
          ),
        );
        await Future<void>.delayed(const Duration(milliseconds: 50));
      });
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));
    }

    testWidgets('is a transparent passthrough on non-desktop platforms', (
      tester,
    ) async {
      // flutter_test's default target platform is already android — no
      // override needed to prove the passthrough.
      final fake = FakePeerBeam();
      await open(tester, fake);

      expect(find.byType(DropTarget), findsNothing);
    });

    // `debugDefaultTargetPlatformOverride` must be back to null before this
    // test's own callback RETURNS: `testWidgets` runs a foundation-debug-var
    // invariant check immediately after the callback, before `package:test`'s
    // `addTearDown` queue is unwound — so an `addTearDown`-based reset runs
    // too late and trips that check on the very next test. A synchronous
    // `try`/`finally` around the body is what actually resets it in time.
    void desktopTest(
      String description,
      Future<void> Function(WidgetTester tester) body,
    ) {
      testWidgets(description, (tester) async {
        debugDefaultTargetPlatformOverride = TargetPlatform.linux;
        try {
          await body(tester);
        } finally {
          debugDefaultTargetPlatformOverride = null;
        }
      });
    }

    desktopTest('dropping two files sends two chat file messages to that '
        'peer', (tester) async {
      final a = _tempFile(tmp, 'a.bin', 'aaaa');
      final b = _tempFile(tmp, 'b.bin', 'bb');
      final fake = FakePeerBeam();
      await open(tester, fake);

      await drop(tester, [a.path, b.path]);

      expect(fake.calls.where((c) => c.startsWith('chatSendFile:')), [
        'chatSendFile:${a.path}',
        'chatSendFile:${b.path}',
      ]);
      // Filed under the peer the drop actually landed on.
      final rows = fake.chatHistories['pb-bob']!;
      expect(rows.map((m) => m.localPath), [a.path, b.path]);
      expect(find.byType(SnackBar), findsNothing);
    });

    desktopTest(
      'dropping a folder sends nothing and surfaces a message naming it',
      (tester) async {
        final folder = Directory('${tmp.path}/vacation photos')..createSync();
        final fake = FakePeerBeam();
        await open(tester, fake);

        await drop(tester, [folder.path]);

        expect(fake.calls.where((c) => c.startsWith('chatSendFile:')), isEmpty);
        expect(find.byType(SnackBar), findsOneWidget);
        expect(find.textContaining('vacation photos'), findsOneWidget);
      },
    );

    desktopTest(
      'dropping one file and one folder sends the file and reports the '
      'folder, in a single message',
      (tester) async {
        final file = _tempFile(tmp, 'a.bin', 'x');
        final folder = Directory('${tmp.path}/notes')..createSync();
        final fake = FakePeerBeam();
        await open(tester, fake);

        await drop(tester, [file.path, folder.path]);

        expect(fake.calls.where((c) => c.startsWith('chatSendFile:')), [
          'chatSendFile:${file.path}',
        ]);
        // One snackbar, not one per folder, and the file was not held back by
        // the folder alongside it.
        expect(find.byType(SnackBar), findsOneWidget);
        expect(find.textContaining('notes'), findsOneWidget);
      },
    );

    desktopTest(
      'dropping onto a peer with no known address sends nothing and says so',
      (tester) async {
        // The composer already disables attach for this peer because the
        // engine refuses the send before anything is enqueued. A drop is the
        // same act by a different gesture, so it must refuse the same way —
        // otherwise the user gets a row of failed bubbles to dismiss, having
        // been told nothing beforehand.
        const addressless = PeerTarget(
          id: 'pb-bob',
          name: 'Bob',
          addresses: [],
          port: 0,
        );
        final file = _tempFile(tmp, 'a.bin', 'x');
        final fake = FakePeerBeam();
        final state = AppState.live(fake);
        addTearDown(state.dispose);
        await tester.pumpWidget(
          AppScope(
            state: state,
            child: const MaterialApp(
              home: ChatScreen(peerId: 'pb-bob', peer: addressless),
            ),
          ),
        );
        await tester.pump();
        await tester.pump(const Duration(milliseconds: 300));

        await drop(tester, [file.path]);

        expect(fake.calls.where((c) => c.startsWith('chatSendFile:')), isEmpty);
        // Scoped to the SnackBar: the composer already shows its own
        // "No address known for Bob" notice for this peer, so an unscoped
        // finder would pass on that alone and prove nothing about the drop.
        expect(
          find.descendant(
            of: find.byType(SnackBar),
            matching: find.textContaining('No address known for Bob'),
          ),
          findsOneWidget,
        );
      },
    );
  });

  // The shell wraps the whole app in the Send flow's `DropZone`, so an open
  // conversation always has TWO drop targets stacked over it. `desktop_drop`
  // delivers to every mounted target rather than only the innermost, which is
  // why the inner one claims ownership and the outer stands down.
  group('a conversation inside the shell owns the drop', () {
    /// The chat screen as the shell really mounts it: inside `DropZone`.
    Future<AppState> openNested(WidgetTester tester, FakePeerBeam fake) async {
      final state = AppState.live(fake);
      addTearDown(state.dispose);
      await tester.pumpWidget(
        AppScope(
          state: state,
          child: MaterialApp(
            home: DropZone(
              staging: state.staging,
              child: const ChatScreen(peerId: 'pb-bob', peer: _peer),
            ),
          ),
        ),
      );
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));
      return state;
    }

    testWidgets('the outer Send zone stands down while a chat is open', (
      tester,
    ) async {
      debugDefaultTargetPlatformOverride = TargetPlatform.linux;
      try {
        final fake = FakePeerBeam();
        await openNested(tester, fake);

        // Both targets are mounted; only the inner one is live.
        final targets = tester.widgetList<DropTarget>(find.byType(DropTarget));
        expect(targets, hasLength(2));
        expect(
          targets.map((t) => t.enable),
          [false, true],
          reason: 'outer (Send) disabled, inner (chat) enabled',
        );
      } finally {
        debugDefaultTargetPlatformOverride = null;
      }
    });

    testWidgets('a drop is answered once: sent to the peer, never staged', (
      tester,
    ) async {
      debugDefaultTargetPlatformOverride = TargetPlatform.linux;
      try {
        final file = _tempFile(tmp, 'holiday.bin', 'xyz');
        final fake = FakePeerBeam();
        final state = await openNested(tester, fake);

        // Drive EVERY mounted target with the same drop — including the
        // disabled outer one. The platform would not call a disabled target,
        // but invoking it anyway is what proves the handler's own claim guard,
        // which is what covers the two frames between a chat claiming drops
        // and the deferred rebuild actually flipping `enable`.
        await tester.runAsync(() async {
          for (final t in tester.widgetList<DropTarget>(
            find.byType(DropTarget),
          )) {
            t.onDragDone!(
              DropDoneDetails(
                files: [DropItemFile(file.path)],
                localPosition: Offset.zero,
                globalPosition: Offset.zero,
              ),
            );
          }
          await Future<void>.delayed(const Duration(milliseconds: 50));
        });
        await tester.pump();
        await tester.pump(const Duration(milliseconds: 300));

        expect(fake.calls.where((c) => c.startsWith('chatSendFile:')), [
          'chatSendFile:${file.path}',
        ]);
        // The Send flow neither staged a second copy nor opened its sheet.
        expect(state.staging.count, 0);
        expect(find.text('Send files'), findsNothing);
      } finally {
        debugDefaultTargetPlatformOverride = null;
      }
    });

    testWidgets('the Send zone takes drops back once the chat is gone', (
      tester,
    ) async {
      debugDefaultTargetPlatformOverride = TargetPlatform.linux;
      try {
        final fake = FakePeerBeam();
        final state = AppState.live(fake);
        addTearDown(state.dispose);
        Widget app(bool chatOpen) => AppScope(
          state: state,
          child: MaterialApp(
            home: DropZone(
              staging: state.staging,
              child: chatOpen
                  ? const ChatScreen(peerId: 'pb-bob', peer: _peer)
                  : const Scaffold(body: Text('home')),
            ),
          ),
        );
        await tester.pumpWidget(app(true));
        // Three frames: the claim is taken after the first, the rebuild that
        // flips `enable` is deferred once more so it never runs mid-build.
        await tester.pump();
        await tester.pump();
        await tester.pump(const Duration(milliseconds: 300));
        expect(
          tester.widget<DropTarget>(find.byType(DropTarget).first).enable,
          isFalse,
        );

        // Leaving the conversation releases the claim: a release that leaked
        // would leave the Send flow permanently deaf to drops.
        await tester.pumpWidget(app(false));
        await tester.pump();
        await tester.pump();
        await tester.pump(const Duration(milliseconds: 300));
        final remaining = tester.widgetList<DropTarget>(
          find.byType(DropTarget),
        );
        expect(remaining, hasLength(1));
        expect(remaining.single.enable, isTrue);
      } finally {
        debugDefaultTargetPlatformOverride = null;
      }
    });
  });
}
