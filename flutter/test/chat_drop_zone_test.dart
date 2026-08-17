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

import 'package:peerbeam/app/theme.dart' show Breakpoints;
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

    // THE CASE UNMOUNTING THE CHAT CANNOT REACH. The app shell is a
    // `StatefulShellRoute.indexedStack`, which keeps EVERY navigation branch
    // mounted — an inactive one is merely offstage — so "the chat screen is
    // mounted" and "the user is looking at it" are different questions, and a
    // drop rides on the second one. go_router mounts each branch as
    // `Offstage(offstage: !isActive, child: TickerMode(enabled: isActive, …))`
    // (14.8.1, `lib/src/route.dart`), which is exactly what is built here.
    //
    // Replacing the chat with another widget — what the test below does — is a
    // different scenario entirely and passes either way: it is the
    // still-mounted-but-offstage case that sent a user's file to whichever
    // peer's thread happened to be open.
    group('a branch the user has navigated away from', () {
      /// One branch of the shell, mounted the way go_router mounts it.
      Widget branch(bool active, Widget child) => Offstage(
        offstage: !active,
        child: TickerMode(enabled: active, child: child),
      );

      /// The shell at [index]: Home in branch 0, an open conversation in
      /// branch 1, both mounted, all of it inside the Send flow's `DropZone`.
      Widget shell(AppState state, int index) => AppScope(
        state: state,
        child: MaterialApp(
          home: DropZone(
            staging: state.staging,
            child: IndexedStack(
              index: index,
              children: [
                branch(index == 0, const Scaffold(body: Text('home'))),
                branch(
                  index == 1,
                  const ChatScreen(peerId: 'pb-bob', peer: _peer),
                ),
              ],
            ),
          ),
        ),
      );

      DropTarget outerTarget(WidgetTester tester) =>
          tester.widget<DropTarget>(find.byType(DropTarget).first);

      /// `skipOffstage: false` throughout — the whole point of these tests is
      /// the branch that is offstage yet still mounted, and the default
      /// finders would simply not see it, which is a way of missing the bug
      /// rather than a way of proving it fixed.
      final chatScreen = find.byType(ChatScreen, skipOffstage: false);

      DropTarget chatTarget(WidgetTester tester) => tester.widget<DropTarget>(
        find.descendant(
          of: chatScreen,
          matching: find.byType(DropTarget, skipOffstage: false),
          skipOffstage: false,
        ),
      );

      /// Settle the shell at [index]: the claim is reconciled during the
      /// chat's own build, and `DropZone` defers the rebuild that flips
      /// `enable` to after the frame.
      Future<void> showBranch(
        WidgetTester tester,
        AppState state,
        int index,
      ) async {
        await tester.pumpWidget(shell(state, index));
        await tester.pump();
        await tester.pump(const Duration(milliseconds: 300));
      }

      testWidgets('leaves the Send zone owning drops again, and stands its own '
          'target down', (tester) async {
        debugDefaultTargetPlatformOverride = TargetPlatform.linux;
        try {
          final fake = FakePeerBeam();
          final state = AppState.live(fake);
          addTearDown(state.dispose);

          await showBranch(tester, state, 1);
          expect(
            outerTarget(tester).enable,
            isFalse,
            reason: 'the conversation the user is looking at owns drops',
          );

          // Tap Home. The conversation is not closed — it is merely offstage.
          await showBranch(tester, state, 0);
          expect(chatScreen, findsOneWidget, reason: 'still mounted');
          expect(
            outerTarget(tester).enable,
            isTrue,
            reason: 'a drop on Home belongs to the Send flow again',
          );
          expect(
            chatTarget(tester).enable,
            isFalse,
            reason: 'and the offstage conversation answers nothing',
          );
        } finally {
          debugDefaultTargetPlatformOverride = null;
        }
      });

      testWidgets('sends nothing when its own handler is driven anyway', (
        tester,
      ) async {
        debugDefaultTargetPlatformOverride = TargetPlatform.linux;
        try {
          final file = _tempFile(tmp, 'tax-return.pdf', 'private');
          final fake = FakePeerBeam();
          final state = AppState.live(fake);
          addTearDown(state.dispose);

          await showBranch(tester, state, 1);
          await showBranch(tester, state, 0);

          // The platform would not call a disabled target, but `desktop_drop`
          // decides who to notify from paint bounds alone — which an offstage
          // `IndexedStack` child still passes, since it is laid out at full
          // size. So the handler itself must refuse, not just `enable`.
          await tester.runAsync(() async {
            chatTarget(tester).onDragDone!(
              DropDoneDetails(
                files: [DropItemFile(file.path)],
                localPosition: Offset.zero,
                globalPosition: Offset.zero,
              ),
            );
            await Future<void>.delayed(const Duration(milliseconds: 50));
          });
          await tester.pump();
          await tester.pump(const Duration(milliseconds: 300));

          expect(
            fake.calls.where((c) => c.startsWith('chatSendFile:')),
            isEmpty,
            reason: 'a file dropped on Home never goes to an offstage peer',
          );
        } finally {
          debugDefaultTargetPlatformOverride = null;
        }
      });

      testWidgets('takes the claim back when the user returns to it', (
        tester,
      ) async {
        debugDefaultTargetPlatformOverride = TargetPlatform.linux;
        try {
          final fake = FakePeerBeam();
          final state = AppState.live(fake);
          addTearDown(state.dispose);

          await showBranch(tester, state, 1);
          await showBranch(tester, state, 0);
          expect(outerTarget(tester).enable, isTrue);

          await showBranch(tester, state, 1);
          expect(
            outerTarget(tester).enable,
            isFalse,
            reason: 'the conversation owns drops again the moment it is back',
          );
          expect(chatTarget(tester).enable, isTrue);
        } finally {
          debugDefaultTargetPlatformOverride = null;
        }
      });
    });

    // THE REGISTER IS NOT A CONSTANT. `AppShell` places its `DropZone` in a
    // different slot below and above `Breakpoints.compact` — the `Scaffold`'s
    // `body` on a narrow window, inside a `Row` beside the navigation rail on a
    // wide one — so dragging the window across 600px destroys `_DropZoneState`
    // and the claim register it owns, while the branch Navigator's GlobalKey
    // carries the open conversation over into the replacement.
    //
    // A zone that bound its register once therefore went on holding a claim on
    // a notifier nobody reads any more: the new Send zone came up enabled
    // beside the still-live chat one, a drop was answered TWICE (sent to the
    // peer and staged for the Send flow), and closing the conversation
    // afterwards threw `A ValueNotifier<int> was used after being disposed`.
    group('the shell rebuilding its DropZone into another slot', () {
      /// `AppShell`'s two layouts, at the real breakpoint, around whichever
      /// [content] the shell is showing. Only the slot the `DropZone` sits in
      /// matters here, so the navigation affordances are stand-ins.
      Widget shell(AppState state, Widget content) => AppScope(
        state: state,
        child: MaterialApp(
          home: Builder(
            builder: (context) {
              final body = DropZone(staging: state.staging, child: content);
              if (MediaQuery.sizeOf(context).width < Breakpoints.compact) {
                return Scaffold(
                  body: body,
                  bottomNavigationBar: const SizedBox(height: 48),
                );
              }
              return Scaffold(
                body: Row(
                  children: [
                    const SizedBox(width: 72),
                    const VerticalDivider(width: 1, thickness: 1),
                    Expanded(child: body),
                  ],
                ),
              );
            },
          ),
        ),
      );

      Future<void> resizeTo(WidgetTester tester, double width) async {
        tester.view.physicalSize = Size(width, 900);
        tester.view.devicePixelRatio = 1.0;
        // Three frames: the resize rebuilds the shell and the chat reclaims
        // during its own build, and `DropZone` defers the rebuild that flips
        // `enable` to after that frame.
        await tester.pump();
        await tester.pump();
        await tester.pump(const Duration(milliseconds: 300));
      }

      testWidgets('leaves exactly one live target, still the chat\'s, and a '
          'drop is still answered once', (tester) async {
        debugDefaultTargetPlatformOverride = TargetPlatform.linux;
        addTearDown(tester.view.reset);
        try {
          final file = _tempFile(tmp, 'holiday.bin', 'xyz');
          final fake = FakePeerBeam();
          final state = AppState.live(fake);
          addTearDown(state.dispose);
          // The GlobalKey the branch Navigator plays in production: the chat is
          // carried into the rebuilt shell rather than mounted afresh, which is
          // precisely what leaves a State holding a register nobody owns.
          final chat = GlobalKey();
          final content = KeyedSubtree(
            key: chat,
            child: const ChatScreen(peerId: 'pb-bob', peer: _peer),
          );

          tester.view.physicalSize = const Size(500, 900);
          tester.view.devicePixelRatio = 1.0;
          await tester.pumpWidget(shell(state, content));
          await tester.pump();
          await tester.pump(const Duration(milliseconds: 300));
          final before = tester.state(find.byType(ChatScreen));

          await resizeTo(tester, 900); // across Breakpoints.compact

          expect(
            tester.state(find.byType(ChatScreen)),
            same(before),
            reason: 'the conversation was carried over, not remounted',
          );
          final targets = tester
              .widgetList<DropTarget>(find.byType(DropTarget))
              .toList();
          expect(targets, hasLength(2), reason: 'outer Send zone and the chat');
          expect(
            targets.map((t) => t.enable),
            [false, true],
            reason: 'the new register was claimed, so the Send zone stood down',
          );

          // Drive every target, disabled ones included — the handler guards are
          // what cover the frames before `enable` catches up.
          await tester.runAsync(() async {
            for (final t in targets) {
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
          expect(
            state.staging.count,
            0,
            reason: 'the Send flow must not stage a second copy of it',
          );
        } finally {
          debugDefaultTargetPlatformOverride = null;
        }
      });

      // The reconciliation rule the shell harness above cannot see, because a
      // GlobalKey re-parent always happens inside the frame that disposes the
      // old zone, so the stale notifier is still alive at that instant. Stated
      // on the register itself: the claim on a replaced register is DROPPED,
      // never decremented. The register belongs to a `DropZone` that is being
      // disposed — touching it is precisely the "used after being disposed"
      // throw — and the count it would correct is going away with it.
      testWidgets('a claim on a replaced register is dropped, not released', (
        tester,
      ) async {
        debugDefaultTargetPlatformOverride = TargetPlatform.linux;
        try {
          final state = AppState.live(FakePeerBeam());
          addTearDown(state.dispose);
          final first = ValueNotifier<int>(0);
          final second = ValueNotifier<int>(0);
          addTearDown(first.dispose);
          addTearDown(second.dispose);

          Widget app(ValueNotifier<int> register) => AppScope(
            state: state,
            child: MaterialApp(
              home: DropClaims(
                claims: register,
                child: const ChatScreen(peerId: 'pb-bob', peer: _peer),
              ),
            ),
          );

          await tester.pumpWidget(app(first));
          await tester.pump();
          await tester.pump(const Duration(milliseconds: 300));
          expect(first.value, 1, reason: 'the open conversation claimed it');

          await tester.pumpWidget(app(second));
          await tester.pump();
          await tester.pump(const Duration(milliseconds: 300));

          expect(
            second.value,
            1,
            reason: 'claimed afresh on the register that now exists',
          );
          expect(
            first.value,
            1,
            reason: 'and the replaced one was never touched again',
          );
        } finally {
          debugDefaultTargetPlatformOverride = null;
        }
      });

      testWidgets('and closing the conversation afterwards throws nothing', (
        tester,
      ) async {
        debugDefaultTargetPlatformOverride = TargetPlatform.linux;
        addTearDown(tester.view.reset);
        try {
          final fake = FakePeerBeam();
          final state = AppState.live(fake);
          addTearDown(state.dispose);
          final chat = GlobalKey();

          tester.view.physicalSize = const Size(500, 900);
          tester.view.devicePixelRatio = 1.0;
          await tester.pumpWidget(
            shell(
              state,
              KeyedSubtree(
                key: chat,
                child: const ChatScreen(peerId: 'pb-bob', peer: _peer),
              ),
            ),
          );
          await tester.pump();
          await tester.pump(const Duration(milliseconds: 300));

          await resizeTo(tester, 900);

          // Leaving the conversation releases the claim — on the register that
          // still exists. Releasing the one the resize disposed is the throw.
          await tester.pumpWidget(
            shell(state, const Scaffold(body: Text('home'))),
          );
          await tester.pump();
          await tester.pump(const Duration(milliseconds: 300));

          expect(tester.takeException(), isNull);
          final remaining = tester.widgetList<DropTarget>(
            find.byType(DropTarget),
          );
          expect(remaining, hasLength(1));
          expect(
            remaining.single.enable,
            isTrue,
            reason: 'and the Send flow has its drops back',
          );
        } finally {
          debugDefaultTargetPlatformOverride = null;
        }
      });
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
