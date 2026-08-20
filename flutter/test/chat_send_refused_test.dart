// A message the engine refused must not quietly disappear.
//
// `chatSend` enqueues durably, so an unreachable peer is *not* a failure: the
// message waits in the engine's outbox and the drain retries it until it lands.
// A refusal is a different thing — an over-long body, an engine that never
// started — and nothing is persisted for it, so no status event will ever
// settle it and no drain will ever carry it.
//
// The optimistic bubble used to be left exactly as it was: `pending`, which
// reads as still on its way. Then the next `refresh` rebuilt the thread from
// engine history, which has no record of a message the engine declined, and the
// row vanished. The user's last sight of their message was a bubble that looked
// like it had gone.

import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/data/chat_repository.dart';
import 'package:peerbeam/sdk/models.dart';

import 'sdk/fake_peerbeam.dart';

const _peer = PeerTarget(
  id: 'pb-bob',
  name: 'Bob',
  addresses: ['127.0.0.1'],
  port: 49600,
);

void main() {
  test(
    'a refused message is failed in place, with the engine\'s own reason',
    () async {
      final fake = FakePeerBeam()..failChatSend = true;
      final chat = ChatRepository(api: fake);
      addTearDown(chat.dispose);

      await chat.send('pb-bob', _peer, 'the invoice is attached');

      final sent = chat.messagesFor('pb-bob');
      expect(sent, hasLength(1));
      expect(sent.single.body, 'the invoice is attached');
      expect(
        sent.single.status,
        ChatStatusValue.failed,
        reason: 'a refused message left pending reads as still on its way',
      );
      expect(
        chat.errorFor(sent.single.id),
        contains('too long'),
        reason: 'the engine said why; the user is the one who can act on it',
      );
      expect(
        chat.isUnsent('pb-bob', sent.single.id),
        isTrue,
        reason: 'nothing was persisted, so only the user can clear this row',
      );
    },
  );

  test('and it survives the refresh that used to erase it', () async {
    final fake = FakePeerBeam()..failChatSend = true;
    final chat = ChatRepository(api: fake);
    addTearDown(chat.dispose);

    await chat.send('pb-bob', _peer, 'can you confirm?');
    // The reconcile: a sibling send, an incoming message, or simply reopening
    // the thread. It rebuilds the conversation from what the engine has — and
    // the engine has nothing for a message it refused.
    await chat.refresh('pb-bob');

    final after = chat.messagesFor('pb-bob');
    expect(after, hasLength(1), reason: 'the row must outlive the reconcile');
    expect(after.single.body, 'can you confirm?');
    expect(after.single.status, ChatStatusValue.failed);
  });

  test('a non-PeerBeam failure is accounted for too', () async {
    final fake = FakePeerBeam()..failing.add('chatSend');
    final chat = ChatRepository(api: fake);
    addTearDown(chat.dispose);

    await chat.send('pb-bob', _peer, 'anyone there?');

    final sent = chat.messagesFor('pb-bob');
    expect(sent.single.status, ChatStatusValue.failed);
    expect(chat.errorFor(sent.single.id), isNotNull);
  });

  test('the user can clear a refused row, and only the user', () async {
    final fake = FakePeerBeam()..failChatSend = true;
    final chat = ChatRepository(api: fake);
    addTearDown(chat.dispose);

    await chat.send('pb-bob', _peer, 'never left');
    final id = chat.messagesFor('pb-bob').single.id;

    chat.dismiss('pb-bob', id);

    expect(chat.messagesFor('pb-bob'), isEmpty);
    expect(chat.errorFor(id), isNull);
    await chat.refresh('pb-bob');
    expect(
      chat.messagesFor('pb-bob'),
      isEmpty,
      reason: 'a dismissed row must not be resurrected by the next reconcile',
    );
  });

  // The other direction, so the fix cannot grow into marking queued messages
  // failed: a send the engine *accepted* is on its way, whether or not the peer
  // is reachable, and must carry no failure at all.
  test('a message the engine accepted is not marked failed', () async {
    final fake = FakePeerBeam();
    final chat = ChatRepository(api: fake);
    addTearDown(chat.dispose);

    await chat.send('pb-bob', _peer, 'on its way');

    final sent = chat.messagesFor('pb-bob');
    expect(sent, hasLength(1));
    expect(sent.single.status, isNot(ChatStatusValue.failed));
    expect(chat.errorFor(sent.single.id), isNull);
    expect(chat.isUnsent('pb-bob', sent.single.id), isFalse);
  });
}
