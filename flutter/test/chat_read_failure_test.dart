// A read that failed and a thing that is empty must never render the same.
//
// Every chat read used to be flattened to emptiness: a thread the engine could
// not return said "No messages yet", a conversation list that never came back
// said "No conversations yet", and — the worst of the three — a search that
// never ran said "No messages match 'x'". That last one is an answer about the
// user's own history, delivered in the voice of a fact, and it is the answer
// they act on by giving up. See `ErrorState`'s own note: an absence is a fact
// about the world, a failure is a fact about us, and only the second comes with
// something to do about it.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/data/chat_repository.dart';
import 'package:peerbeam/features/chat/chat_screen.dart';
import 'package:peerbeam/features/chats/chats_screen.dart';
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

ChatMessage _text(String peerId, String id, String body) => ChatMessage(
  id: id,
  peerId: peerId,
  direction: 'in',
  body: body,
  at: DateTime.now(),
  status: ChatStatusValue.received,
);

Future<AppState> _pump(
  WidgetTester tester,
  FakePeerBeam fake,
  Widget home,
) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: MaterialApp(home: home),
    ),
  );
  // Fixed pumps rather than `pumpAndSettle`: the app state keeps timers
  // running, so "settled" never arrives.
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 300));
  return state;
}

Future<void> _tapRetry(WidgetTester tester) async {
  await tester.tap(find.text('Try again'));
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 300));
}

void main() {
  testWidgets('a thread the engine could not read is not shown as an empty '
      'conversation', (tester) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [_text('pb-bob', 'm1', 'the invoice')];
    fake.failing.add('chatHistory');
    await _pump(tester, fake, const ChatScreen(peerId: 'pb-bob', peer: _peer));

    expect(find.text('Could not open this conversation'), findsOneWidget);
    expect(
      find.text('No messages yet'),
      findsNothing,
      reason: 'a failed read claimed the conversation is empty',
    );

    // The retry is the same act as opening the thread, and it is the whole
    // point of saying so rather than staying silent.
    fake.failing.remove('chatHistory');
    await _tapRetry(tester);

    expect(find.text('the invoice'), findsOneWidget);
    expect(find.text('Could not open this conversation'), findsNothing);
  });

  // Messages already on screen are worth more than an error page over them: a
  // reload that fails leaves the thread readable, which is what makes a local
  // history local.
  testWidgets('a reload that fails keeps the messages already read', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [_text('pb-bob', 'm1', 'the invoice')];
    final state = await _pump(
      tester,
      fake,
      const ChatScreen(peerId: 'pb-bob', peer: _peer),
    );
    expect(find.text('the invoice'), findsOneWidget);

    fake.failing.add('chatHistory');
    await state.chat.refresh('pb-bob');
    await tester.pump();

    expect(find.text('the invoice'), findsOneWidget);
    expect(find.text('Could not open this conversation'), findsNothing);
  });

  testWidgets('a conversation list the engine could not read is not shown as '
      'no conversations', (tester) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [_text('pb-bob', 'm1', 'hello')];
    fake.failing.add('chatConversations');
    await _pump(tester, fake, const ChatsScreen());

    expect(find.text('Could not read your conversations'), findsOneWidget);
    expect(
      find.text('No conversations yet'),
      findsNothing,
      reason: 'a failed read hid every thread behind a claim there are none',
    );

    fake.failing.remove('chatConversations');
    await _tapRetry(tester);

    expect(find.text('pb-bob'), findsOneWidget);
    expect(find.text('Could not read your conversations'), findsNothing);
  });

  testWidgets('a search that failed never says the message is not there', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      _text('pb-bob', 'm1', 'the quarterly invoice'),
    ];
    fake.failing.add('chatSearch');
    await _pump(tester, fake, const ChatsScreen());

    await tester.enterText(find.byType(TextField), 'invoice');
    await tester.pump(const Duration(milliseconds: 300));
    await tester.pump();

    expect(find.text('Could not search your messages'), findsOneWidget);
    expect(
      find.textContaining('No messages match'),
      findsNothing,
      reason: 'a search that never ran told the user their message is gone',
    );

    // Retried as the same query — the user asked for these results once, and
    // should not have to retype to ask again.
    fake.failing.remove('chatSearch');
    await _tapRetry(tester);

    expect(find.text('the quarterly invoice'), findsOneWidget);
    expect(find.text('Could not search your messages'), findsNothing);
  });

  // The contract that made the false negative possible, pinned where it lives:
  // an empty result set is the engine stating the message is not there, and a
  // search that never ran has stated nothing at all.
  test(
    'a failed search throws rather than answering "nothing found"',
    () async {
      final fake = FakePeerBeam();
      fake.failing.add('chatSearch');
      final repo = ChatRepository(api: fake);
      addTearDown(repo.dispose);

      await expectLater(repo.search('invoice'), throwsA(isA<StateError>()));
    },
  );

  // Per peer, and cleared by the read that succeeds: one thread that could not
  // be read must not make the thread beside it look broken, and must not go on
  // looking broken itself once it comes back.
  test(
    'a read error is kept per conversation and cleared when it answers',
    () async {
      final fake = FakePeerBeam();
      fake.chatHistories['pb-bob'] = [_text('pb-bob', 'm1', 'hello')];
      fake.failing.add('chatHistory');
      final repo = ChatRepository(api: fake);
      addTearDown(repo.dispose);

      await repo.refresh('pb-bob');
      expect(repo.loadErrorFor('pb-bob'), isNotNull);
      expect(repo.loadErrorFor('pb-alice'), isNull);

      fake.failing.remove('chatHistory');
      await repo.refresh('pb-bob');

      expect(repo.loadErrorFor('pb-bob'), isNull);
      expect(repo.messagesFor('pb-bob'), hasLength(1));
    },
  );

  test(
    'a conversation-list error is cleared by the list that answers',
    () async {
      final fake = FakePeerBeam();
      fake.chatHistories['pb-bob'] = [_text('pb-bob', 'm1', 'hello')];
      fake.failing.add('chatConversations');
      final repo = ChatRepository(api: fake);
      addTearDown(repo.dispose);

      await repo.refreshConversations();
      expect(repo.conversationsError, isNotNull);
      expect(repo.conversations, isEmpty);

      fake.failing.remove('chatConversations');
      await repo.refreshConversations();

      expect(repo.conversationsError, isNull);
      expect(repo.conversations, hasLength(1));
    },
  );
}
