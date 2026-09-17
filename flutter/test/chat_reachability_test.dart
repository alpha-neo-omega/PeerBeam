// Controls a person can actually reach: with a thumb, with a keyboard, with a
// screen reader.
//
// Each of these was reachable only one way, and the one way excluded somebody.

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/chat/chat_screen.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

const _peer = PeerTarget(
  id: 'pb-bob',
  name: 'Bob',
  addresses: ['10.0.0.2'],
  port: 49600,
);

ChatMessage _msg({List<ChatReaction> reactions = const []}) => ChatMessage(
  id: 'm1',
  peerId: 'pb-bob',
  direction: 'in',
  body: 'shall I delete the backups?',
  at: null,
  storedAt: DateTime.utc(2026, 1, 1),
  status: ChatStatusValue.received,
  reactions: reactions,
);

Future<void> _open(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: const MaterialApp(
        home: ChatScreen(peerId: 'pb-bob', peer: _peer),
      ),
    ),
  );
  for (var i = 0; i < 4; i++) {
    await tester.pump(const Duration(milliseconds: 200));
  }
}

void main() {
  // A ~18px chip inside the bubble's own tap area: a miss did not do nothing,
  // it hit the bubble behind — which on a received file row opens the file with
  // the OS handler, and on a slightly long press starts selection instead.
  testWidgets('a reaction chip fills a real tap target', (tester) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      _msg(
        reactions: const [ChatReaction(emoji: '👍', by: 'in')],
      ),
    ];
    await _open(tester, fake);

    final chip = find.ancestor(
      of: find.text('👍'),
      matching: find.byType(ConstrainedBox),
    );
    expect(chip, findsWidgets);
    final box = tester.getSize(find.byType(InkWell).at(0));
    expect(
      tester.getSize(chip.first).height,
      greaterThanOrEqualTo(kMinInteractiveDimension),
      reason: 'the thumb needs a box the eye does not',
    );
    expect(box, isNotNull);
  });

  // "👍 2" says nothing about what tapping does, and the difference between
  // adding and withdrawing is carried purely by a 1px border.
  testWidgets('a reaction chip says what tapping it will do', (tester) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      _msg(
        reactions: const [ChatReaction(emoji: '👍', by: 'out')],
      ),
    ];
    await _open(tester, fake);

    expect(
      find.bySemanticsLabel(RegExp('tap to withdraw')),
      findsOneWidget,
      reason: 'your own reaction announces that tapping removes it',
    );
  });

  testWidgets("someone else's reaction announces that tapping adds one", (
    tester,
  ) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      _msg(
        reactions: const [ChatReaction(emoji: '👍', by: 'in')],
      ),
    ];
    await _open(tester, fake);

    expect(find.bySemanticsLabel(RegExp('tap to react')), findsOneWidget);
  });

  // Selection is the gateway to Reply, Copy, Forward and Delete for a single
  // message, and every way in was a pointer gesture: long-press on touch,
  // right-click on desktop. A keyboard-only user could reach none of them.
  testWidgets('selection can be started from the keyboard', (tester) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [_msg()];
    await _open(tester, fake);

    // Nothing selected yet: the app bar is the ordinary one.
    expect(find.byTooltip('Reply'), findsNothing);

    // Tab the way a keyboard user does, trying the shortcut at each stop. The
    // bubble is one of several focusable things on the screen and its position
    // in the traversal order is not something this test should pin.
    var started = false;
    for (var i = 0; i < 14 && !started; i++) {
      await tester.sendKeyEvent(LogicalKeyboardKey.tab);
      await tester.pump();
      await tester.sendKeyDownEvent(LogicalKeyboardKey.shiftLeft);
      await tester.sendKeyEvent(LogicalKeyboardKey.enter);
      await tester.sendKeyUpEvent(LogicalKeyboardKey.shiftLeft);
      await tester.pump();
      started = find.byTooltip('Reply').evaluate().isNotEmpty;
    }

    expect(
      started,
      isTrue,
      reason: 'Shift+Enter on a focused bubble must start selection',
    );
  });
}
