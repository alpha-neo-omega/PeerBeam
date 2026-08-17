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
}
