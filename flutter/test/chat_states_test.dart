// The three states a conversation can be in, and the two it must never
// confuse.
//
// "No messages yet" and "Nothing said yet" are statements about what the user
// and their peers have written. A surface may only make them from a read that
// came back. Two other states look identical in the data — an empty list — and
// mean entirely different things:
//
//   * the read has not happened yet, and
//   * the read failed.
//
// Rendering either as emptiness tells someone their conversation is gone.

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/data/chat_repository.dart';
import 'package:peerbeam/data/groups_repository.dart';
import 'package:peerbeam/features/chat/chat_screen.dart';
import 'package:peerbeam/features/groups/group_chat_screen.dart';
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

const _group = Group(
  id: 'g1',
  name: 'Team',
  members: ['pb-bob'],
  reachable: ['pb-bob'],
);

/// A fake whose history read never answers, so the un-read state can actually
/// be looked at. With the ordinary fake the read completes inside the first
/// pump, which is precisely why this state went unnoticed.
class _NeverAnswers extends FakePeerBeam {
  final gate = Completer<List<ChatMessage>>();

  @override
  Future<List<ChatMessage>> chatHistory(String peerId) => gate.future;
}

void main() {
  group('a one-to-one thread', () {
    test('is not "loaded" until it has actually been read', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);
      addTearDown(repo.dispose);

      expect(repo.hasLoaded('pb-bob'), isFalse);
      await repo.refresh('pb-bob');
      expect(repo.hasLoaded('pb-bob'), isTrue);
    });

    // A failed read must not be mistaken for a completed one, or the screen
    // goes straight from the spinner to claiming the thread is empty.
    test('a read that failed does not count as loaded', () async {
      final fake = FakePeerBeam()..failing.add('chatHistory');
      final repo = ChatRepository(api: fake);
      addTearDown(repo.dispose);

      await repo.refresh('pb-bob');
      expect(repo.hasLoaded('pb-bob'), isFalse);
      expect(repo.loadErrorFor('pb-bob'), isNotNull);
    });

    // The first frame, before the post-frame read has run. Deliberately a peer
    // with NO history, which is the case that used to be indistinguishable:
    // with messages the screen is right either way, and with none it claimed
    // emptiness it had not yet established.
    testWidgets('an unread thread shows progress, not "No messages yet"', (
      tester,
    ) async {
      final fake = _NeverAnswers();
      final state = AppState.live(fake);
      addTearDown(state.dispose);
      addTearDown(() => fake.gate.complete(const []));

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

      expect(find.text('No messages yet'), findsNothing);
      expect(find.byType(CircularProgressIndicator), findsWidgets);
    });

    testWidgets('a genuinely empty thread does say so, once read', (
      tester,
    ) async {
      final fake = FakePeerBeam(); // no history for this peer
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
      for (var i = 0; i < 6; i++) {
        await tester.pump(const Duration(milliseconds: 200));
      }

      expect(find.text('No messages yet'), findsOneWidget);
    });
  });

  group('a group transcript', () {
    test('a failed read is reported, not returned as emptiness', () async {
      final fake = FakePeerBeam()..failing.add('groupHistory');
      final repo = GroupsRepository(api: fake);
      addTearDown(repo.dispose);

      final read = await repo.history('g1');
      expect(read.messages, isEmpty);
      expect(
        read.error,
        isNotNull,
        reason: 'the caller cannot tell empty from unreadable otherwise',
      );
    });

    test('a read that came back empty reports no error', () async {
      final fake = FakePeerBeam();
      final repo = GroupsRepository(api: fake);
      addTearDown(repo.dispose);

      final read = await repo.history('g1');
      expect(read.messages, isEmpty);
      expect(read.error, isNull);
    });

    testWidgets('an unreadable group says so instead of "Nothing said yet"', (
      tester,
    ) async {
      final fake = FakePeerBeam()..failing.add('groupHistory');
      final state = AppState.live(fake);
      addTearDown(state.dispose);

      await tester.pumpWidget(
        AppScope(
          state: state,
          child: MaterialApp(
            home: GroupChatScreen(group: _group, nameFor: (id) => id),
          ),
        ),
      );
      for (var i = 0; i < 6; i++) {
        await tester.pump(const Duration(milliseconds: 200));
      }

      expect(find.text('Nothing said yet'), findsNothing);
      expect(find.text('Could not open this group'), findsOneWidget);
    });

    testWidgets('an empty group still says nothing has been said', (
      tester,
    ) async {
      final fake = FakePeerBeam();
      final state = AppState.live(fake);
      addTearDown(state.dispose);

      await tester.pumpWidget(
        AppScope(
          state: state,
          child: MaterialApp(
            home: GroupChatScreen(group: _group, nameFor: (id) => id),
          ),
        ),
      );
      for (var i = 0; i < 6; i++) {
        await tester.pump(const Duration(milliseconds: 200));
      }

      expect(find.text('Nothing said yet'), findsOneWidget);
    });
  });
  group('the conversations list', () {
    test('is not "loaded" until the engine has answered', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);
      addTearDown(repo.dispose);

      expect(repo.conversationsLoaded, isFalse);
      await repo.refreshConversations();
      expect(repo.conversationsLoaded, isTrue);
    });

    test('a failed read does not count as loaded', () async {
      final fake = FakePeerBeam()..failing.add('chatConversations');
      final repo = ChatRepository(api: fake);
      addTearDown(repo.dispose);

      await repo.refreshConversations();
      expect(repo.conversationsLoaded, isFalse);
      expect(repo.conversationsError, isNotNull);
    });
  });
}
