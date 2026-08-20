// Replying to a message, and the case that matters: the answered message is
// gone.
//
// A reply carries only the answered message's **id**. Nothing quotes text into
// the new body, because a snapshot would outlive the message it quoted — which
// is exactly how a disappearing-message window gets defeated by anyone replying
// to something.
//
// So the parent is resolved against the rows already on screen, and when it is
// not among them the reply renders as an orphan. It still shows its marker:
// hiding the reply would delete one message because a *different* one was
// deleted, and dropping just the marker is worse, since "sure, go ahead"
// answering *shall I delete the backups?* and *can I borrow a pen?* are the
// same seven characters.

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

ChatMessage _msg(
  String id,
  String body, {
  String? replyTo,
  String dir = 'in',
}) => ChatMessage(
  id: id,
  peerId: 'pb-bob',
  direction: dir,
  body: body,
  at: DateTime.now(),
  status: 'delivered',
  inReplyTo: replyTo,
);

Future<AppState> _open(WidgetTester tester, FakePeerBeam fake) async {
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
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 300));
  return state;
}

String _copy(WidgetTester tester) => tester
    .widgetList<Text>(find.byType(Text))
    .map((t) => t.data ?? '')
    .join(' ');

void main() {
  testWidgets('a reply shows what it is answering', (tester) async {
    final fake = FakePeerBeam()
      ..chatHistories['pb-bob'] = [
        _msg('m1', 'shall I delete the backups?'),
        _msg('m2', 'sure, go ahead', replyTo: 'm1'),
      ];
    await _open(tester, fake);

    expect(_copy(tester), contains('shall I delete the backups?'));
    expect(_copy(tester), contains('sure, go ahead'));
  });

  testWidgets('an orphaned reply still says it is a reply', (tester) async {
    // The parent is absent — deleted, or disappeared with a window.
    final fake = FakePeerBeam()
      ..chatHistories['pb-bob'] = [_msg('m2', 'sure, go ahead', replyTo: 'm1')];
    await _open(tester, fake);

    expect(
      _copy(tester),
      contains('no longer here'),
      reason: 'without the marker there is no way to know what was answered',
    );
    expect(
      _copy(tester),
      contains('sure, go ahead'),
      reason: 'the reply itself must not vanish because its parent did',
    );
  });

  testWidgets('an ordinary message carries no reply marker', (tester) async {
    final fake = FakePeerBeam()
      ..chatHistories['pb-bob'] = [_msg('m1', 'just a message')];
    await _open(tester, fake);
    expect(_copy(tester), isNot(contains('no longer here')));
  });

  testWidgets('selecting one message offers Reply; selecting two does not', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..chatHistories['pb-bob'] = [_msg('m1', 'first'), _msg('m2', 'second')];
    await _open(tester, fake);

    await tester.longPress(find.text('first'));
    await tester.pumpAndSettle();
    expect(find.byTooltip('Reply'), findsOneWidget);

    // A reply names a single message, so replying to two means nothing.
    await tester.tap(find.text('second'));
    await tester.pumpAndSettle();
    expect(
      find.byTooltip('Reply'),
      findsNothing,
      reason: 'offering it disabled would pose a question with no answer',
    );
  });

  testWidgets('the reply banner shows the quote and can be cancelled', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..chatHistories['pb-bob'] = [_msg('m1', 'shall I delete the backups?')];
    await _open(tester, fake);

    await tester.longPress(find.text('shall I delete the backups?'));
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('Reply'));
    await tester.pumpAndSettle();

    expect(_copy(tester), contains('Replying to'));

    await tester.tap(find.byTooltip('Stop replying'));
    await tester.pumpAndSettle();
    expect(_copy(tester), isNot(contains('Replying to')));
  });

  testWidgets('sending a reply passes the id, never the quoted text', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..chatHistories['pb-bob'] = [_msg('m1', 'shall I delete the backups?')];
    await _open(tester, fake);

    await tester.longPress(find.text('shall I delete the backups?'));
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('Reply'));
    await tester.pumpAndSettle();

    await tester.enterText(find.byType(TextField), 'sure, go ahead');
    await tester.testTextInput.receiveAction(TextInputAction.done);
    await tester.pumpAndSettle();

    expect(
      fake.calls,
      contains('chatSend:sure, go ahead:reply=m1'),
      reason: 'the reference travels; the quoted text must not',
    );
    // And the banner is gone, so the next message is not silently a reply too.
    expect(_copy(tester), isNot(contains('Replying to')));
  });
}
