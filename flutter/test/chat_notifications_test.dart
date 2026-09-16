// When an arriving message interrupts someone, and what it is allowed to say.
//
// A notification is drawn by the operating system, outside anything a widget
// test can reach — so the decision and the copy are pure functions, and this is
// where they are checked. `desktop_notifier.dart` is the part that cannot be
// tested here, and it deliberately contains no rules: it posts what it is
// given.
//
// Two properties matter more than the rest. A notification must not tell
// someone what they are already reading, and it must not put more of a private
// conversation on a lock screen than is needed to recognise it.

import 'dart:async';

import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/platform/chat_notifications.dart';
import 'package:peerbeam/platform/chat_notifier.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/chat_presence.dart';

ChatMessage _msg({
  String id = 'm1',
  String peer = 'pb-bob',
  String direction = 'in',
  String body = 'hello',
  String status = ChatStatusValue.received,
  String kind = ChatMessageKind.text,
  String? fileName,
  String? group,
}) => ChatMessage(
  id: id,
  peerId: peer,
  direction: direction,
  body: body,
  at: null,
  storedAt: DateTime.utc(2026, 1, 1),
  status: status,
  kind: kind,
  fileName: fileName,
  group: group,
);

bool _notify(ChatMessage m, {String? open, bool focused = true}) =>
    shouldNotifyForMessage(m, openConversation: open, windowFocused: focused);

void main() {
  group('whether to notify at all', () {
    test('an ordinary inbound message does', () {
      expect(_notify(_msg()), isTrue);
    });

    test('our own message does not', () {
      expect(_notify(_msg(direction: 'out')), isFalse);
    });

    // An outgoing row settling from `pending` to `sent` comes back through the
    // same event. It is a status change, not an arrival.
    test('an outbound row that merely settled does not', () {
      expect(
        _notify(_msg(direction: 'out', status: ChatStatusValue.sent)),
        isFalse,
      );
      expect(_notify(_msg(status: ChatStatusValue.sent)), isFalse);
    });

    test('a group message does', () {
      expect(_notify(_msg(group: 'g1')), isTrue);
    });

    group('and the thread on screen', () {
      test('the open, focused thread is not notified about', () {
        expect(_notify(_msg(), open: 'pb-bob'), isFalse);
      });

      test('a different open thread still is', () {
        expect(_notify(_msg(), open: 'pb-carol'), isTrue);
      });

      // The half that is easy to get wrong. A thread left open behind a locked
      // screen, another window, or a backgrounded app is exactly the case a
      // notification exists for — "open" alone is not "being read".
      test('the open thread IS notified about when the app is not focused', () {
        expect(_notify(_msg(), open: 'pb-bob', focused: false), isTrue);
      });

      test('no thread open, app unfocused, still notified', () {
        expect(_notify(_msg(), open: null, focused: false), isTrue);
      });

      // The mistake the key exists to prevent. A group row is not a private
      // message from whoever sent it: reading Bob's private thread must not
      // silence the group he also posted in, and reading the group must not
      // silence his private thread.
      test('a private thread does not silence a group the sender posts in', () {
        expect(_notify(_msg(group: 'g1'), open: 'pb-bob'), isTrue);
      });

      test('the open group is not notified about', () {
        expect(_notify(_msg(group: 'g1'), open: 'group:g1'), isFalse);
      });

      test('an open group does not silence the sender private thread', () {
        expect(_notify(_msg(), open: 'group:g1'), isTrue);
      });

      test('a different open group still notifies', () {
        expect(_notify(_msg(group: 'g1'), open: 'group:g2'), isTrue);
      });
    });
  });

  group('the conversation key', () {
    test('a one-to-one thread is keyed by the peer', () {
      expect(chatThreadKey(_msg()), 'pb-bob');
    });

    test('a group is keyed by the group, behind a prefix', () {
      expect(chatThreadKey(_msg(group: 'g1')), 'group:g1');
      expect(groupThreadKey('g1'), 'group:g1');
    });

    // Not luck: the engine's store refuses a namespace containing a colon, so
    // no device id this app can hold a conversation with contains one. It is
    // the same rule that makes `ts:<node>` unusable as a chat key.
    test('a peer key can never look like a group key', () {
      expect(chatThreadKey(_msg(peer: 'pb-bob')).startsWith('group:'), isFalse);
    });

    test('the two never share a notification id', () {
      expect(chatNoticeId('g1'), isNot(chatNoticeId(groupThreadKey('g1'))));
    });
  });

  group('what it says', () {
    test('the peer name is the title', () {
      final n = chatNotice(_msg(), peerName: 'Bob’s Laptop');
      expect(n.title, 'Bob’s Laptop');
      expect(n.body, 'hello');
      expect(n.threadKey, 'pb-bob');
    });

    // Ugly, but true. A made-up friendly name for a device we cannot name
    // would be worse than the id the user can at least match against a list.
    test('an unknown peer falls back to the device id, not to a guess', () {
      expect(chatNotice(_msg(), peerName: '').title, 'pb-bob');
      expect(chatNotice(_msg(), peerName: '   ').title, 'pb-bob');
    });

    test('a long message is cut, and says it was', () {
      final long = 'x' * (kChatPreviewChars + 50);
      final body = chatNotice(_msg(body: long), peerName: 'Bob').body;

      expect(body.length, lessThanOrEqualTo(kChatPreviewChars + 1));
      expect(body, endsWith('…'));
      // The cap is the point: the rest of the message stays in the app.
      expect(body.contains(long), isFalse);
    });

    test('a message exactly at the cap is not cut', () {
      final exact = 'y' * kChatPreviewChars;
      expect(chatNotice(_msg(body: exact), peerName: 'Bob').body, exact);
    });

    // A notification is one line almost everywhere. Collapsing here means the
    // truncation above counts the characters that will actually be shown,
    // rather than newlines the OS is about to strip anyway.
    test('newlines and runs of spaces collapse to single spaces', () {
      final n = chatNotice(
        _msg(body: 'line one\n\nline   two\ttab'),
        peerName: 'Bob',
      );
      expect(n.body, 'line one line two tab');
    });

    test('an empty body says something rather than nothing', () {
      expect(
        chatNotice(_msg(body: '   '), peerName: 'Bob').body,
        'New message',
      );
    });

    group('a shared file', () {
      test('is named rather than previewed', () {
        final n = chatNotice(
          _msg(body: '', kind: ChatMessageKind.file, fileName: 'report.pdf'),
          peerName: 'Bob',
        );
        expect(n.body, 'report.pdf');
      });

      // An offer is a question, and reads as one. "report.pdf" alone would
      // suggest it had already arrived.
      test('awaiting a decision says so', () {
        final n = chatNotice(
          _msg(
            body: '',
            kind: ChatMessageKind.file,
            fileName: 'report.pdf',
            status: ChatStatusValue.pendingApproval,
          ),
          peerName: 'Bob',
        );
        expect(n.body, 'Wants to send report.pdf');
      });

      test('a nameless file still says a file arrived', () {
        final n = chatNotice(
          _msg(body: '', kind: ChatMessageKind.file),
          peerName: 'Bob',
        );
        expect(n.body, 'a file');
      });
    });

    group('in a group', () {
      test('the group is the title and the speaker leads the body', () {
        final n = chatNotice(
          _msg(group: 'g1', body: 'standup at ten'),
          peerName: 'Bob',
          groupName: 'Team',
        );
        expect(n.title, 'Team');
        expect(n.body, 'Bob: standup at ten');
        expect(n.threadKey, 'group:g1');
      });

      // Nothing reads the group list until Groups is opened, so an arriving
      // group message on a fresh start has no name to use. A group id is 32
      // hex characters — as a conversation's name on a lock screen it tells
      // the reader nothing and looks like a fault.
      test('an unnamed group gets a heading, never its raw id', () {
        final n = chatNotice(
          _msg(group: '7f3a9c21b04e4d8fa1c6e5720b93d4aa', body: 'ping'),
          peerName: 'Bob',
        );
        expect(n.title, 'New group message');
        expect(n.title, isNot(contains('7f3a')));
        // The part that makes it worth having is still there.
        expect(n.body, 'Bob: ping');
      });

      test('an unnamed speaker falls back to their device id', () {
        final n = chatNotice(
          _msg(group: 'g1', body: 'hi'),
          peerName: '',
          groupName: 'Team',
        );
        expect(n.body, 'pb-bob: hi');
      });

      test('a shared file is described, with who shared it', () {
        final n = chatNotice(
          _msg(
            group: 'g1',
            body: '',
            kind: ChatMessageKind.file,
            fileName: 'notes.md',
          ),
          peerName: 'Bob',
          groupName: 'Team',
        );
        expect(n.body, 'Bob: notes.md');
      });
    });
  });

  group('the notification id', () {
    // Per conversation, not per message: someone who stepped away comes back
    // to one notification per person rather than forty.
    test('is the same for every message from one peer', () {
      final a = chatNotice(_msg(id: 'm1'), peerName: 'Bob');
      final b = chatNotice(
        _msg(id: 'm2', body: 'again'),
        peerName: 'Bob',
      );
      expect(a.id, b.id);
    });

    test('differs between peers', () {
      final a = chatNotice(_msg(peer: 'pb-bob'), peerName: 'Bob');
      final b = chatNotice(_msg(peer: 'pb-carol'), peerName: 'Carol');
      expect(a.id, isNot(b.id));
    });

    // Its own band, clear of the service notification (1) and of received-file
    // ids (0x40000000 and up). Ids also have to survive the trip into a Kotlin
    // Int, so they stay positive and 31-bit.
    test('stays inside its reserved, positive band', () {
      for (final peer in ['pb-bob', '', 'ts:node-1', 'x' * 200]) {
        final id = chatNoticeId(peer);
        expect(id, greaterThanOrEqualTo(0x20000000));
        expect(id, lessThanOrEqualTo(0x2FFFFFFF));
      }
    });

    test('the withdrawal id matches the one posted', () {
      final n = chatNotice(_msg(), peerName: 'Bob');
      expect(chatNoticeId(n.threadKey), n.id);
    });
  });

  group('where the user is looking', () {
    test('entering and leaving a thread', () {
      final p = ChatPresence();
      expect(p.openConversation, isNull);
      expect(p.foreground, isTrue);

      p.enter('pb-bob');
      expect(p.isWatching('pb-bob'), isTrue);
      expect(p.isWatching('pb-carol'), isFalse);

      p.leave('pb-bob');
      expect(p.openConversation, isNull);
    });

    // Pushing one thread on top of another builds the new screen before
    // disposing the old, so the old screen's `leave` can arrive last. Without
    // removing by value it would blank the thread that had just registered —
    // and that thread would then notify about messages the user is reading.
    test('a stale leave does not clear the thread that replaced it', () {
      final p = ChatPresence()..enter('pb-bob');
      p.enter('pb-carol'); // navigated on
      p.leave('pb-bob'); // the old screen's dispose, arriving late

      expect(p.openConversation, 'pb-carol');
    });

    // The defect a single slot had. Chat screens nest — a thread can be opened
    // from inside another — and popping back must reveal the one underneath.
    // With one slot the pop cleared it, so the conversation the user had just
    // been returned to started notifying about itself.
    test('popping back reveals the thread underneath', () {
      final p = ChatPresence()..enter('pb-bob');
      p.enter('pb-carol'); // pushed over Bob
      p.leave('pb-carol'); // popped

      expect(p.openConversation, 'pb-bob');
      expect(p.isWatching('pb-bob'), isTrue);
    });

    test('leaving a buried thread does not change what is in front', () {
      final p = ChatPresence()..enter('pb-bob');
      p.enter('pb-carol');

      var n = 0;
      p.addListener(() => n++);
      p.leave('pb-bob'); // the buried one goes away

      expect(p.openConversation, 'pb-carol');
      expect(n, 0, reason: 'attention did not move');
    });

    test('leaving them all ends with no thread open', () {
      final p = ChatPresence()..enter('pb-bob');
      p.enter('pb-carol');
      p.leave('pb-carol');
      p.leave('pb-bob');

      expect(p.openConversation, isNull);
    });

    test('an open thread is not being watched while backgrounded', () {
      final p = ChatPresence()..enter('pb-bob');
      p.setForeground(false);
      expect(p.isWatching('pb-bob'), isFalse);
    });

    test('only real changes notify', () {
      final p = ChatPresence();
      var n = 0;
      p.addListener(() => n++);

      p.enter('pb-bob');
      p.enter('pb-bob'); // same thread
      p.setForeground(true); // already true
      p.leave('pb-carol'); // not the open one
      expect(n, 1);

      p.setForeground(false);
      p.leave('pb-bob');
      expect(n, 3);
    });
  });

  group('the notifier, end to end', () {
    late StreamController<BridgeEvent> events;
    late ChatPresence presence;
    late List<ChatNotice> posted;
    late List<int> withdrawn;
    late bool enabled;
    late ChatNotifier notifier;

    setUp(() {
      events = StreamController<BridgeEvent>.broadcast();
      presence = ChatPresence();
      posted = [];
      withdrawn = [];
      enabled = true;
      notifier = ChatNotifier(
        events: events.stream,
        presence: presence,
        enabled: () => enabled,
        nameOf: (id) => id == 'pb-bob' ? 'Bob' : '',
        groupNameOf: (id) => id == 'g1' ? 'Team' : '',
        post: (n) async => posted.add(n),
        withdraw: (id) async => withdrawn.add(id),
      )..start();
      addTearDown(() {
        notifier.dispose();
        events.close();
        presence.dispose();
      });
    });

    Future<void> send(ChatMessage m) async {
      events.add(ChatReceived(m));
      await Future(() {});
    }

    test('an arriving message is announced, named after its sender', () async {
      await send(_msg());
      expect(posted, hasLength(1));
      expect(posted.single.title, 'Bob');
      expect(posted.single.body, 'hello');
    });

    // Read live, not captured: a switch turned off has to take effect on the
    // next message, not the next launch.
    test('the notifications setting is honoured as it changes', () async {
      enabled = false;
      await send(_msg());
      expect(posted, isEmpty);

      enabled = true;
      await send(_msg(id: 'm2'));
      expect(posted, hasLength(1));
    });

    test('nothing is announced for the thread being read', () async {
      presence.enter('pb-bob');
      await send(_msg());
      expect(posted, isEmpty);
    });

    // The point of tracking which notice is standing: opening the thread
    // answers it, and leaving it in the notification centre would ask again
    // for something already done.
    test('opening the thread takes its notification down', () async {
      await send(_msg());
      expect(posted, hasLength(1));

      presence.enter('pb-bob');
      expect(withdrawn, [posted.single.id]);
    });

    test('opening a thread with nothing standing withdraws nothing', () async {
      presence.enter('pb-carol');
      expect(withdrawn, isEmpty);
    });

    test('a notice is withdrawn once, not on every presence change', () async {
      await send(_msg());
      presence.enter('pb-bob');
      presence.leave('pb-bob');
      presence.enter('pb-bob');
      expect(withdrawn, hasLength(1));
    });

    // Coming back to a window that was left on the thread is the same
    // situation as opening it: the message has been seen.
    test('returning to the foreground on an open thread withdraws', () async {
      presence.setForeground(false);
      presence.enter('pb-bob');
      await send(_msg());
      expect(posted, hasLength(1), reason: 'backgrounded — still news');

      presence.setForeground(true);
      expect(withdrawn, [posted.single.id]);
    });

    test('losing focus withdraws nothing', () async {
      await send(_msg());
      presence.setForeground(false);
      expect(withdrawn, isEmpty);
    });

    test('a nameless peer is announced under its device id', () async {
      await send(_msg(peer: 'pb-zed'));
      expect(posted.single.title, 'pb-zed');
    });

    test('a group message is announced under the group', () async {
      await send(_msg(group: 'g1'));
      expect(posted.single.title, 'Team');
      expect(posted.single.body, 'Bob: hello');
      expect(posted.single.threadKey, 'group:g1');
    });

    test(
      'reading the group withdraws the group notice, not a peer one',
      () async {
        await send(_msg()); // private
        await send(_msg(id: 'm2', group: 'g1')); // group
        expect(posted, hasLength(2));

        presence.enter('group:g1');
        expect(withdrawn, [chatNoticeId('group:g1')]);

        presence.enter('pb-bob');
        expect(withdrawn, [chatNoticeId('group:g1'), chatNoticeId('pb-bob')]);
      },
    );

    test('unrelated events are ignored', () async {
      events.add(const HistoryUpdated());
      await Future(() {});
      expect(posted, isEmpty);
    });

    test('nothing is watched after dispose', () async {
      notifier.dispose();
      await send(_msg());
      expect(posted, isEmpty);
    });
  });
}
