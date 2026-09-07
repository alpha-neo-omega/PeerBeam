// What a chat row's clock is allowed to say, and what decides where the row
// sits in the transcript.
//
// Two times exist per row. `at` is the sender's own stamp — on an inbound row,
// a peer's unvalidated claim about its own clock. `storedAt` is when THIS
// device wrote the row.
//
// A surface uses `storedAt` for BOTH what it shows and what it sorts by
// (`shownAt`), and that single number is what these tests guard. Splitting the
// two — label from the sender, order from here — is what an earlier version of
// this file asserted, and it puts a row in one place while it claims another:
// a message a peer queued while offline then reads "08:00" underneath a bubble
// reading "14:30".

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/data/chat_repository.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/features/chat/chat_screen.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

Future<void> flush() => Future(() {});

ChatMessage _msg(
  String id, {
  String peer = 'pb-bob',
  String direction = 'in',
  DateTime? at,
  DateTime? storedAt,
  String status = ChatStatusValue.received,
}) => ChatMessage(
  id: id,
  peerId: peer,
  direction: direction,
  body: id,
  at: at,
  storedAt: storedAt,
  status: status,
);

void main() {
  group('a time is never invented', () {
    // The defect: `at` was `DateTime.tryParse(...) ?? DateTime.now()`. A peer
    // controls that string and nothing on the wire validates it, so an
    // unparseable one did not read as "unknown" — it read as the current
    // clock, which is a wrong answer rather than a missing one, and it was
    // re-minted on every parse, so the same bubble showed a later time after
    // every reload.
    test('an unparseable timestamp is null, not now', () {
      final msg = ChatMessage.fromJson(const {
        'id': 'm1',
        'peer_id': 'pb-bob',
        'direction': 'in',
        'timestamp': 'not a timestamp',
        'body': 'hi',
        'status': 'received',
      });

      expect(msg.at, isNull);
      expect(msg.storedAt, isNull);
      expect(msg.shownAt, isNull, reason: 'nothing to show is not "now"');
      expect(msg.orderedAt, isNull);
    });

    test('a missing timestamp key is null, not now', () {
      expect(ChatMessage.fromJson(const {'id': 'm1'}).at, isNull);
    });

    // Parsing the same bytes twice must produce the same row. Under the old
    // fallback it could not: two parses a second apart disagreed by a second.
    test('parsing the same record twice yields the same time', () {
      const j = {'id': 'm1', 'timestamp': 'junk', 'body': 'hi'};
      expect(ChatMessage.fromJson(j).at, ChatMessage.fromJson(j).at);
    });

    test('a reaction with an unparseable timestamp is null, not now', () {
      expect(
        ChatReaction.fromJson(const {'emoji': '👍', 'by': 'in'}).at,
        isNull,
      );
    });

    // The honest fallback: when the sender's stamp is unusable, when the row
    // got HERE is a real answer, and it is this device's own clock.
    test('a junk timestamp still displays the local arrival time', () {
      final msg = ChatMessage.fromJson(const {
        'id': 'm1',
        'timestamp': 'junk',
        'stored_at': '2026-01-01T10:00:00Z',
        'body': 'hi',
      });

      expect(msg.at, isNull);
      expect(msg.shownAt, DateTime.parse('2026-01-01T10:00:00Z'));
      expect(msg.orderedAt, DateTime.parse('2026-01-01T10:00:00Z'));
    });
  });

  group('the time shown is the time ordered by', () {
    final sent = DateTime.parse('2099-01-01T00:00:00Z');
    final arrived = DateTime.parse('2026-01-01T10:00:00Z');

    // The invariant, and the reason `shownAt` is one getter rather than two.
    // A row that is sorted by one number and labelled with another sits in one
    // place while claiming another, and no rule can make that read correctly:
    // from the receiver's side "sent long ago, delivered late" and "sent now by
    // a device whose clock is behind" are the same two numbers.
    test('a peer claiming another year is both shown and ordered locally', () {
      final msg = _msg('m1', at: sent, storedAt: arrived);

      expect(msg.shownAt, arrived);
      expect(msg.orderedAt, arrived);
      expect(msg.shownAt, msg.orderedAt);
      // The claim is still carried, for anything that wants it AS a claim.
      expect(msg.at, sent);
    });

    test('an ordinary row has one instant anyway', () {
      final msg = _msg('m1', direction: 'out', at: arrived, storedAt: arrived);
      expect(msg.shownAt, msg.orderedAt);
    });

    test('with no local stamp it falls back to the sender\'s', () {
      final msg = _msg('m1', at: sent);
      expect(msg.shownAt, sent);
      expect(msg.orderedAt, sent);
    });
  });

  group('copyWith carries every field it does not replace', () {
    // `copyWith` documents that an omitted argument never clears a field, and
    // was dropping `group`. A status settling on a group row is the ordinary
    // path for an outgoing one, and it turned that row back into a one-to-one
    // message — the difference between appearing in the group's transcript and
    // appearing in a private thread with whoever sent it.
    test('a group row that settles is still a group row', () {
      final row = ChatMessage(
        id: 'm1',
        peerId: 'pb-bob',
        direction: 'out',
        body: 'hi',
        at: DateTime.parse('2026-01-01T10:00:00Z'),
        status: ChatStatusValue.pending,
        group: 'g-1',
      );

      expect(row.copyWith(status: ChatStatusValue.sent).group, 'g-1');
    });

    test('storedAt survives a status change', () {
      final at = DateTime.parse('2026-01-01T10:00:00Z');
      final row = _msg('m1', at: at, storedAt: at, status: 'pending');
      expect(row.copyWith(status: 'sent').storedAt, at);
    });
  });

  group('the conversations list reads recency from the local clock', () {
    test('last_at wins over the peer-supplied last_timestamp', () {
      final c = ChatConversation.fromJson(const {
        'peer_id': 'pb-bob',
        'last_timestamp': '2099-01-01T00:00:00Z',
        'last_at': '2026-01-01T10:00:00Z',
      });

      expect(c.lastAt, DateTime.parse('2026-01-01T10:00:00Z'));
    });

    test('an engine too old to report last_at still dates the thread', () {
      final c = ChatConversation.fromJson(const {
        'peer_id': 'pb-bob',
        'last_timestamp': '2026-01-01T10:00:00Z',
      });

      expect(c.lastAt, DateTime.parse('2026-01-01T10:00:00Z'));
    });
  });

  group('transcript order', () {
    // A message a peer queued while offline arrives stamped with its ORIGINAL
    // send time (the outbox flush carries `entry.timestamp` —
    // peerbeam-chat/src/send.rs) but is stored on arrival
    // (`ChatRecord::received` stamps `Utc::now()` unconditionally —
    // peerbeam-chat/src/record.rs). So `at` is old and `storedAt` is now, and
    // this test uses those values rather than the impossible pair an earlier
    // version of it asserted (an old `storedAt`, which no engine path can
    // produce — the test passed and proved nothing).
    //
    // It lands last, which is right: it is the most recent thing to reach this
    // device. What matters is that its LABEL agrees with that position rather
    // than reading 08:00 underneath a bubble reading 14:30 — which is what
    // `shownAt` guarantees by being the same number the sort uses.
    test('a drained outbox lands last and is labelled with its arrival, not '
        'its send time', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);
      final mine = DateTime.parse('2026-01-01T14:29:00Z');
      final drainedAt = DateTime.parse('2026-01-01T14:30:00Z');
      fake.chatHistories['pb-bob'] = [
        _msg('recent', direction: 'out', at: mine, storedAt: mine),
      ];
      await repo.refresh('pb-bob');

      // Composed at 08:00-08:05 while this peer was offline; all three reach
      // us at 14:30.
      for (final t in ['08:00', '08:02', '08:05']) {
        fake.emit(
          ChatReceived(
            _msg(
              't$t',
              at: DateTime.parse('2026-01-01T$t:00Z'),
              storedAt: drainedAt,
            ),
          ),
        );
      }
      await flush();

      final rows = repo.messagesFor('pb-bob');
      expect(
        rows.map((m) => m.id).toList(),
        ['recent', 't08:00', 't08:02', 't08:05'],
        reason: 'the drained burst did not land at the newest end',
      );
      // And no bubble claims a time that contradicts where it sits: the times
      // read top-to-bottom are non-decreasing.
      final shown = rows.map((m) => m.shownAt!).toList();
      for (var i = 1; i < shown.length; i++) {
        expect(
          shown[i].isBefore(shown[i - 1]),
          isFalse,
          reason:
              'row $i is labelled ${shown[i]} but sits below ${shown[i - 1]}',
        );
      }
    });

    test('a live arrival that IS newest still goes last', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);
      fake.chatHistories['pb-bob'] = [
        _msg(
          'older',
          at: DateTime.parse('2026-01-01T10:00:00Z'),
          storedAt: DateTime.parse('2026-01-01T10:00:00Z'),
        ),
      ];
      await repo.refresh('pb-bob');

      fake.emit(
        ChatReceived(
          _msg(
            'newest',
            at: DateTime.parse('2026-01-01T11:00:00Z'),
            storedAt: DateTime.parse('2026-01-01T11:00:00Z'),
          ),
        ),
      );
      await flush();

      expect(repo.messagesFor('pb-bob').last.id, 'newest');
    });

    // A peer whose clock runs fast must not place its messages above ones sent
    // after them. Ordering is by `storedAt`, so the claim is ignored.
    test('a peer claiming the future does not jump the transcript', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);
      fake.chatHistories['pb-bob'] = [
        _msg(
          'liar',
          at: DateTime.parse('2099-01-01T00:00:00Z'),
          storedAt: DateTime.parse('2026-01-01T10:00:00Z'),
        ),
        _msg(
          'honest',
          direction: 'out',
          at: DateTime.parse('2026-01-01T11:00:00Z'),
          storedAt: DateTime.parse('2026-01-01T11:00:00Z'),
        ),
      ];

      await repo.refresh('pb-bob');

      expect(
        repo.messagesFor('pb-bob').map((m) => m.id).toList(),
        ['liar', 'honest'],
        reason: "the peer's claimed send time decided the order",
      );
    });

    test(
      'a disappearing-message window that closes mid-view is swept',
      () async {
        final fake = FakePeerBeam();
        final repo = ChatRepository(api: fake);
        fake.retention['pb-bob'] = 3600;
        fake.chatHistories['pb-bob'] = [
          _msg('m1', at: DateTime.parse('2026-01-01T10:00:00Z')),
        ];
        await repo.openThread('pb-bob');
        expect(repo.messagesFor('pb-bob').length, 1);

        // The window closes: the engine stops returning the row (its `history`
        // is what enforces the window) and a prune deletes it from disk.
        fake.chatHistories['pb-bob'] = [];
        fake.calls.clear();

        await repo.sweepRetention('pb-bob');

        expect(
          repo.messagesFor('pb-bob'),
          isEmpty,
          reason: 'an expired message stayed on screen',
        );
        expect(fake.calls, contains('pruneChat:pb-bob'));
      },
    );

    test('a thread with no window is not swept at all', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);
      fake.chatHistories['pb-bob'] = [
        _msg('m1', at: DateTime.parse('2026-01-01T10:00:00Z')),
      ];
      await repo.openThread('pb-bob');
      fake.calls.clear();

      await repo.sweepRetention('pb-bob');

      // No window means nothing can have expired, so the sweep must not cost
      // a prune or a re-read — this is what a timer would otherwise tick
      // forever on a thread that keeps its messages.
      expect(fake.calls, isEmpty);
      expect(repo.messagesFor('pb-bob').length, 1);
    });

    // THE REPORTED SYMPTOM: "new chat above old chat", inside one thread.
    //
    // A peer whose clock runs 3 hours behind mints its message ids and
    // timestamps 3 hours in the past. The engine returns a thread in store-key
    // (message id) order, so that peer's REPLY — genuinely the newest message
    // — comes back ahead of the message it is answering, and the transcript
    // showed it above. `storedAt` is when each row reached this device, which
    // is the same clock for both, so ordering by it restores what happened.
    test('a peer whose clock runs behind does not appear above the message '
        'it is answering', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);
      // The order the engine hands over: by message id, i.e. by each author's
      // own clock. The peer's id is 3h in the past, so it sorts first.
      fake.chatHistories['pb-bob'] = [
        _msg(
          'their-reply',
          at: DateTime.parse('2026-01-01T11:00:00Z'), // their skewed clock
          storedAt: DateTime.parse('2026-01-01T14:00:00Z'), // actually arrived
        ),
        _msg(
          'my-question',
          direction: 'out',
          at: DateTime.parse('2026-01-01T13:00:00Z'),
          storedAt: DateTime.parse('2026-01-01T13:00:00Z'),
        ),
      ];

      await repo.refresh('pb-bob');

      expect(
        repo.messagesFor('pb-bob').map((m) => m.id).toList(),
        ['my-question', 'their-reply'],
        reason: 'the reply still sits above the question it answers',
      );
    });

    // A row this device cannot date at all is kept, and gets a DEFINITE
    // position rather than a comparison that contradicts itself. Falling back
    // to comparing ids is not a valid ordering: with `b` undatable, ids can
    // place `a` after `b` and `b` after `c` while the two datable rows' times
    // place `a` before `c`, and `List.sort` may then return any arrangement.
    // Undatable sorts oldest, matching what the engine does with the same row.
    test('an undatable row sorts oldest and is never dropped', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);
      // Ids chosen so an id-based tiebreak would contradict the times: by id
      // alone this is c, b, a; by time a comes before c.
      fake.chatHistories['pb-bob'] = [
        _msg('c', at: DateTime.parse('2026-01-01T10:00:00Z')),
        _msg('b'),
        _msg('a', at: DateTime.parse('2026-01-01T12:00:00Z')),
      ];

      await repo.refresh('pb-bob');

      expect(
        repo.messagesFor('pb-bob').map((m) => m.id).toList(),
        ['b', 'c', 'a'],
        reason: 'the undatable row did not sort oldest',
      );
    });

    // The property behind that example: the comparator must be a total order,
    // so the arrangement cannot depend on the order rows happened to arrive in.
    test('the transcript order does not depend on input order', () async {
      final rows = [
        _msg('c', at: DateTime.parse('2026-01-01T10:00:00Z')),
        _msg('b'),
        _msg('a', at: DateTime.parse('2026-01-01T12:00:00Z')),
        _msg('d', at: DateTime.parse('2026-01-01T11:00:00Z')),
        _msg('e'),
      ];

      // Every rotation of the same rows must yield one arrangement.
      final expected = <String>[];
      for (var i = 0; i < rows.length; i++) {
        final rotated = [...rows.sublist(i), ...rows.sublist(0, i)];
        final fake = FakePeerBeam();
        final repo = ChatRepository(api: fake);
        fake.chatHistories['pb-bob'] = rotated;
        await repo.refresh('pb-bob');
        final got = repo.messagesFor('pb-bob').map((m) => m.id).toList();
        if (expected.isEmpty) {
          expected.addAll(got);
        } else {
          expect(got, expected, reason: 'rotation $i ordered differently');
        }
      }
      // Undatable rows first (by id), then the datable ones by time.
      expect(expected, ['b', 'e', 'c', 'd', 'a']);
    });
  });

  group('what the screen actually renders', () {
    // The transcript is a `reverse: true` ListView that ALSO indexes
    // `items[length - 1 - i]` (chat_screen.dart:827-832). Two reversals that
    // cancel out are easy to "fix" into one, so this pins the outcome by
    // measured y-coordinate rather than by reading the widget tree: oldest at
    // the top, newest at the bottom, like every messenger.
    testWidgets('the newest message is at the BOTTOM', (tester) async {
      final fake = FakePeerBeam();
      fake.chatHistories['pb-bob'] = [
        _screenMsg('OLDEST', '10:00'),
        _screenMsg('MIDDLE', '11:00'),
        _screenMsg('NEWEST', '12:00'),
      ];
      final state = AppState.live(fake);
      addTearDown(state.dispose);
      await tester.pumpWidget(
        AppScope(
          state: state,
          child: const MaterialApp(
            home: ChatScreen(peerId: 'pb-bob', peer: _probePeer),
          ),
        ),
      );
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));

      final oldest = tester.getTopLeft(find.text('OLDEST')).dy;
      final middle = tester.getTopLeft(find.text('MIDDLE')).dy;
      final newest = tester.getTopLeft(find.text('NEWEST')).dy;

      expect(
        oldest < middle && middle < newest,
        isTrue,
        reason:
            'y: OLDEST=$oldest MIDDLE=$middle NEWEST=$newest — the transcript '
            'is not oldest-at-top',
      );
    });
  });
}

const _probePeer = PeerTarget(
  id: 'pb-bob',
  name: 'Bob',
  addresses: ['127.0.0.1'],
  port: 49600,
);

ChatMessage _screenMsg(String body, String hhmm) => ChatMessage(
  id: 'id-$hhmm',
  peerId: 'pb-bob',
  direction: 'in',
  body: body,
  at: DateTime.parse('2026-01-01T$hhmm:00Z'),
  storedAt: DateTime.parse('2026-01-01T$hhmm:00Z'),
  status: ChatStatusValue.received,
);
