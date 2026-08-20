// Disappearing messages, as a conversation surface has to present them.
//
// The engine's guarantee is narrow and true: a message is readable on THIS
// device for at most the window, and is then deleted from this device. Every
// test here defends one of the ways this screen could widen that into a promise
// PeerBeam cannot keep, because the copy on the other device is on the other
// device's disk:
//
//  1. **The limit is stated where the conversation is read**, not in a help
//     page. A user who believes "disappearing" means both sides has been misled
//     by the UI, which is worse than not shipping the feature.
//  2. **Received files are not deleted, and off is not an undo.** Both are
//     stated before a window is chosen, in the sheet that chooses it.
//  3. **A window that could not be read is never rendered as "off".** Off is
//     the default, so an absence is a claim — "nothing here disappears" — and
//     it is the one wrong answer this surface can give about a conversation
//     that may be deleting itself hourly.
//
// The window is also reported in the engine's own numbers: choosing one deletes
// history immediately, and can delete a message that was still waiting to be
// sent, which is a thing the user has to be told rather than left to discover.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/chat/chat_screen.dart';
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

/// A fake whose window READ fails.
///
/// [FakePeerBeam.failing] cannot produce this one — `chatRetention` does not
/// consult it — and it is the failure that matters most here, so the test
/// brings its own. [readFails] is flipped to prove the retry actually re-reads.
class _WindowUnreadable extends FakePeerBeam {
  bool readFails = true;

  @override
  Future<int?> chatRetention(String peerId) async {
    calls.add('chatRetention:$peerId');
    if (readFails) throw StateError('fake failure: chatRetention');
    return retention[peerId];
  }
}

/// A fake whose prune actually removes something, so the surface can be held to
/// reporting the engine's counts rather than a restatement of what was asked.
class _PrunesPlenty extends FakePeerBeam {
  @override
  Future<({int messages, int queued})> pruneChat({String? peerId}) async {
    calls.add('pruneChat:${peerId ?? 'all'}');
    return (messages: 3, queued: 1);
  }
}

Widget _screen(AppState state) => AppScope(
  state: state,
  child: const MaterialApp(
    home: ChatScreen(peerId: 'pb-bob', peer: _peer),
  ),
);

/// Pump the screen and let its post-frame `openThread` — reconcile, window
/// read, prune, history read — land.
Future<AppState> _open(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(_screen(state));
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 300));
  return state;
}

/// Open the disappearing-messages sheet from the app bar. By tooltip, never by
/// icon: the strip carries the same timer glyph the action does.
Future<void> _openSheet(WidgetTester tester) async {
  await tester.tap(find.byTooltip('Disappearing messages'));
  await tester.pumpAndSettle();
}

/// Every string on screen, joined — for the copy assertions, where the claim is
/// about what the user can read rather than about which widget says it.
String _copy(WidgetTester tester) => tester
    .widgetList<Text>(find.byType(Text))
    .map((t) => t.data ?? '')
    .join(' ');

ChatMessage _text(String id, String body) => ChatMessage(
  id: id,
  peerId: 'pb-bob',
  direction: 'in',
  body: body,
  at: DateTime.now(),
  status: ChatStatusValue.received,
);

void main() {
  /// The load-bearing test. A window in force is stated above the thread, and
  /// the sentence next to it is the limit — anything less leaves "disappearing"
  /// to be read as "deleted on both sides".
  testWidgets(
    'a window in force is stated in the thread, as this device only',
    (tester) async {
      final fake = FakePeerBeam()
        ..retention['pb-bob'] = 3600
        ..chatHistories['pb-bob'] = [_text('m1', 'hello')];
      await _open(tester, fake);

      expect(
        find.text('Messages disappear from this device after 1 hour'),
        findsOneWidget,
      );
      final copy = _copy(tester);
      expect(
        copy,
        contains('Bob keeps its own copy'),
        reason: 'the peer\'s copy has to be named where the thread is read',
      );
      expect(copy, contains('this device cannot delete that'));
      expect(
        copy,
        contains('Received files are kept'),
        reason: 'a deleted row must not be read as a deleted file',
      );
    },
  );

  /// Off is the default, so it is the one state that may render as nothing —
  /// but only because the sheet is where the default is explained.
  testWidgets('off draws no claim above the thread', (tester) async {
    final fake = FakePeerBeam()
      ..chatHistories['pb-bob'] = [_text('m1', 'hello')];
    await _open(tester, fake);

    expect(
      find.textContaining('Messages disappear from this device'),
      findsNothing,
    );
    expect(find.textContaining('Could not check'), findsNothing);
    expect(find.text('hello'), findsOneWidget);
  });

  /// The three limits are in front of the choices, not behind a link. Each one
  /// is a belief a person forms wrongly and cannot recover from.
  testWidgets('the sheet states the local limit, the files limit and that off '
      'is not an undo', (tester) async {
    await _open(tester, FakePeerBeam());
    await _openSheet(tester);

    final copy = _copy(tester);
    expect(copy, contains('This window is local to this device'));
    expect(
      copy,
      contains('does not ask Bob to delete its copy'),
      reason: 'there is no frame that asks, and the sheet may not imply one',
    );
    expect(copy, contains('readable here for at most the window'));
    expect(copy, contains('Received files are not deleted'));
    expect(copy, contains('Off by default'));
    expect(
      copy,
      contains('cannot bring back what has already been deleted'),
      reason: 'turning a window off is not an undo, and must not read as one',
    );
  });

  /// Windows, not a free-text duration — and the current one is named as
  /// current rather than merely tinted.
  testWidgets('the sheet offers four windows and marks the one in force', (
    tester,
  ) async {
    final fake = FakePeerBeam()..retention['pb-bob'] = 86400;
    await _open(tester, fake);
    await _openSheet(tester);

    expect(find.text('Off'), findsOneWidget);
    expect(find.text('1 hour'), findsOneWidget);
    expect(find.text('1 day'), findsOneWidget);
    expect(find.text('7 days'), findsOneWidget);
    expect(find.text('Current'), findsOneWidget);
    final tile = tester.widget<ListTile>(
      find.ancestor(of: find.text('1 day'), matching: find.byType(ListTile)),
    );
    expect(tile.selected, isTrue);
  });

  /// Choosing a window writes it, deletes what it has already closed over, and
  /// reports the engine's counts — including the one nobody would guess: a
  /// message that was waiting to be sent and now never will be.
  testWidgets('choosing a window prunes at once and reports what went, '
      'including what will never be sent', (tester) async {
    final fake = _PrunesPlenty()
      ..chatHistories['pb-bob'] = [_text('m1', 'hello')];
    await _open(tester, fake);
    fake.calls.clear();

    await _openSheet(tester);
    await tester.tap(find.text('1 hour'));
    await tester.pumpAndSettle();

    expect(fake.calls, contains('setChatRetention:pb-bob:3600'));
    expect(
      fake.calls,
      contains('pruneChat:pb-bob'),
      reason: 'a window that only hid rows would be a lie about this disk',
    );
    final copy = _copy(tester);
    expect(copy, contains('3 older messages deleted now'));
    expect(
      copy,
      contains('1 that was waiting to be sent will not be sent'),
      reason:
          'a queued message deleted by a window is a send that will not '
          'happen, which the count alone does not say',
    );
    // And the thread now says what it is set to.
    expect(
      find.text('Messages disappear from this device after 1 hour'),
      findsOneWidget,
    );
  });

  /// Turning it off says the one thing off cannot do, and prunes nothing —
  /// there is nothing whose window has closed, and nothing already deleted is
  /// coming back.
  testWidgets('turning a window off says it is not an undo', (tester) async {
    final fake = FakePeerBeam()..retention['pb-bob'] = 3600;
    await _open(tester, fake);
    fake.calls.clear();

    await _openSheet(tester);
    await tester.tap(find.text('Off'));
    await tester.pumpAndSettle();

    expect(fake.calls, contains('setChatRetention:pb-bob:off'));
    expect(fake.calls, isNot(contains('pruneChat:pb-bob')));
    expect(
      _copy(tester),
      contains('anything already deleted is gone'),
      reason: 'off stops the deleting; it does not reverse it',
    );
    expect(
      find.textContaining('Messages disappear from this device'),
      findsNothing,
    );
  });

  /// Opening a thread with a window set deletes what the engine's reads were
  /// already hiding: filtering keeps the promise on time, pruning keeps it
  /// about bytes.
  testWidgets('opening a thread prunes only when a window is set', (
    tester,
  ) async {
    final withWindow = FakePeerBeam()..retention['pb-bob'] = 604800;
    await _open(tester, withWindow);
    expect(withWindow.calls, contains('pruneChat:pb-bob'));

    final withoutWindow = FakePeerBeam();
    await _open(tester, withoutWindow);
    expect(
      withoutWindow.calls,
      isNot(contains('pruneChat:pb-bob')),
      reason:
          'a conversation with no window has nothing to prune, and asking '
          'costs a write on every open',
    );
  });

  /// A window set from the CLI can be any duration. It is shown in its own
  /// words: a 90-minute window rendered as "1 hour" or "2 hours" states that
  /// messages live for a length of time they do not.
  testWidgets('an odd window is shown exactly, never rounded', (tester) async {
    final fake = FakePeerBeam()..retention['pb-bob'] = 5400;
    await _open(tester, fake);

    expect(
      find.text('Messages disappear from this device after 90 minutes'),
      findsOneWidget,
    );
  });

  /// The failure this whole shape exists for: a read that did not answer must
  /// not render as the default. "No strip" means "nothing here disappears", and
  /// stating that about a conversation nobody could read is the one
  /// unrecoverable claim — the user stops checking.
  testWidgets('a window read that failed is stated, never rendered as off', (
    tester,
  ) async {
    final fake = _WindowUnreadable()
      ..chatHistories['pb-bob'] = [_text('m1', 'hello')];
    await _open(tester, fake);

    expect(
      find.text('Could not check whether messages here disappear'),
      findsOneWidget,
    );
    expect(find.byTooltip('Try again'), findsOneWidget);
    // The messages are still right there — the failed read is about the
    // window, not about the conversation.
    expect(find.text('hello'), findsOneWidget);

    fake.readFails = false;
    fake.retention['pb-bob'] = 86400;
    await tester.tap(find.byTooltip('Try again'));
    await tester.pumpAndSettle();

    expect(find.textContaining('Could not check'), findsNothing);
    expect(
      find.text('Messages disappear from this device after 1 day'),
      findsOneWidget,
    );
  });

  /// And the sheet marks nothing as current when nothing was read: a tick
  /// against "Off" would be a claim about this conversation that no read
  /// established.
  testWidgets('the sheet marks no window as current when the read failed', (
    tester,
  ) async {
    await _open(tester, _WindowUnreadable());
    await _openSheet(tester);

    expect(find.text('Current'), findsNothing);
    expect(
      _copy(tester),
      contains('could not read the window in force here'),
      reason: 'an unmarked list needs to say why it is unmarked',
    );
  });
}
