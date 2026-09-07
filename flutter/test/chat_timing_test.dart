// What a chat row's clock is allowed to say, and what decides where the row
// sits in the transcript.
//
// Two times exist per row and they answer different questions. `at` is the
// sender's own stamp — on an inbound row, a peer's unvalidated claim about its
// own clock. `storedAt` is when THIS device wrote the row. A surface shows the
// first and orders by the second; conflating them is what every test here
// guards against.

import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/data/chat_repository.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/models.dart';

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
      expect(msg.displayAt, isNull, reason: 'nothing to show is not "now"');
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
      expect(msg.displayAt, DateTime.parse('2026-01-01T10:00:00Z'));
      expect(msg.orderedAt, DateTime.parse('2026-01-01T10:00:00Z'));
    });
  });

  group('which clock answers which question', () {
    final sent = DateTime.parse('2099-01-01T00:00:00Z');
    final arrived = DateTime.parse('2026-01-01T10:00:00Z');

    test('display shows the send time, ordering uses the arrival time', () {
      final msg = _msg('m1', at: sent, storedAt: arrived);

      // What the sender said — a transcript's clock answers "when was this
      // sent".
      expect(msg.displayAt, sent);
      // What this device knows — ordering must not be a peer's to choose.
      expect(msg.orderedAt, arrived);
    });

    test('an outgoing row has one instant, so both agree', () {
      final msg = _msg('m1', direction: 'out', at: arrived, storedAt: arrived);
      expect(msg.displayAt, msg.orderedAt);
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
    // The defect: `_onReceived` appended. An arrival is usually the newest
    // thing in the thread — but a peer that was offline queues its messages
    // and flushes them on reconnect, so a burst stamped hours ago landed
    // BELOW messages the user had sent minutes earlier, and then jumped back
    // up on the next refresh.
    test('a drained outbox lands in time order, not at the bottom', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);
      fake.chatHistories['pb-bob'] = [
        _msg(
          'recent',
          direction: 'out',
          at: DateTime.parse('2026-01-01T14:30:00Z'),
          storedAt: DateTime.parse('2026-01-01T14:30:00Z'),
        ),
      ];
      await repo.refresh('pb-bob');

      // Three messages the peer queued while offline, arriving now but
      // stamped — and stored — earlier.
      for (final t in ['08:00', '08:02', '08:05']) {
        fake.emit(
          ChatReceived(
            _msg(
              't$t',
              at: DateTime.parse('2026-01-01T$t:00Z'),
              storedAt: DateTime.parse('2026-01-01T$t:00Z'),
            ),
          ),
        );
      }
      await flush();

      expect(
        repo.messagesFor('pb-bob').map((m) => m.id).toList(),
        ['t08:00', 't08:02', 't08:05', 'recent'],
        reason: 'an arrival was appended rather than placed in time order',
      );
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
}
