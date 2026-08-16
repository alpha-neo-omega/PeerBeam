// The Chats destination: every conversation on disk, whether or not discovery
// can currently see the peer — and the one destructive action on a thread.
//
// These behaviours moved here from `home_selection_test.dart` when
// Conversations left Home for a nav destination of its own; the reasons they
// exist are unchanged, and are restated on each test.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/features/chats/chats_screen.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';
import 'sdk/fake_peerbeam.dart';

const _laptop = SdkDevice(
  id: 'x1',
  name: 'Live Laptop',
  kind: 'laptop',
  platform: 'linux',
  addresses: ['127.0.0.1'],
  port: 49600,
  online: true,
  latencyMs: 5,
  reachableLan: true,
  reachableRemote: false,
);

ChatMessage _text(String peerId, String id, {DateTime? at, String? body}) =>
    ChatMessage(
      id: id,
      peerId: peerId,
      direction: 'in',
      body: body ?? 'hello',
      at: at ?? DateTime.now(),
      status: ChatStatusValue.received,
    );

ChatMessage _queuedFile(String peerId, String id, String name) => ChatMessage(
  id: id,
  peerId: peerId,
  direction: 'out',
  body: '',
  at: DateTime.now(),
  status: ChatStatusValue.pending,
  kind: ChatMessageKind.file,
  fileName: name,
  fileSize: 7,
);

/// Pump the Chats screen against [fake] and let its post-frame refresh run.
Future<AppState> _pumpChats(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(state: state, child: const MaterialApp(home: ChatsScreen())),
  );
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 300));
  return state;
}

void main() {
  // Without this list a thread is only reachable through a device tile — so a
  // file queued for a peer that has since gone offline has no entry point at
  // all, which is exactly the case 2b exists to serve.
  testWidgets('a conversation with a peer discovery cannot see is listed, and '
      'opens under the engine\'s own peer id', (tester) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-ghost'] = [
      _queuedFile('pb-ghost', 'fr-1', 'movie.mkv'),
    ];
    await _pumpChats(tester, fake);

    // Discovery has never heard of this peer, so its id is the only name we
    // can honestly put to it.
    expect(find.text('pb-ghost'), findsOneWidget);

    await tester.tap(find.text('pb-ghost'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    // Opened under the authenticated id the engine returned — the one both
    // halves of the conversation already agree on.
    expect(fake.calls, contains('chatHistory:pb-ghost'));
    // …and the thread is readable while the composer says why it cannot send.
    expect(find.text('movie.mkv'), findsOneWidget);
    expect(find.textContaining('No address known for pb-ghost'), findsOneWidget);
  });

  testWidgets('a discovered peer\'s conversation shows its live name', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    fake.chatHistories['x1'] = [_text('x1', 'm1', body: 'hi')];
    final state = AppState.live(fake);
    addTearDown(state.dispose);
    await tester.pumpWidget(
      AppScope(state: state, child: const MaterialApp(home: ChatsScreen())),
    );
    await tester.pump();
    fake.emit(const DeviceAdded(_laptop));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    // The row carries the live name, not the raw device id.
    expect(find.text('Live Laptop'), findsOneWidget);
    expect(find.text('x1'), findsNothing);
    expect(find.textContaining('Last message'), findsOneWidget);
  });

  // `unread_hint` counts INBOUND FILE OFFERS AWAITING A DECISION. PeerBeam has
  // no read receipts and no local read-state, so a real unread count is not
  // computable — rendering this as "N unread" would be a guess dressed as a
  // fact, and a thread full of unread text legitimately reads 0.
  testWidgets('a waiting offer reads as "needs your attention", never as '
      'unread', (tester) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      ChatMessage(
        id: 'fr-1',
        peerId: 'pb-bob',
        direction: 'in',
        body: '',
        at: DateTime.now(),
        status: ChatStatusValue.pendingApproval,
        kind: ChatMessageKind.file,
        fileName: 'theirs.bin',
        fileSize: 7,
      ),
    ];
    await _pumpChats(tester, fake);

    expect(find.text('1 file offer needs your attention'), findsOneWidget);
    expect(find.textContaining('unread'), findsNothing);
    expect(
      find.byTooltip('1 file offer is waiting for your decision'),
      findsOneWidget,
    );
  });

  // The plural, and the badge that carries the count — still a count of
  // decisions, still never described as unread.
  testWidgets('several waiting offers pluralise and badge the count', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      for (var i = 0; i < 3; i++)
        ChatMessage(
          id: 'fr-$i',
          peerId: 'pb-bob',
          direction: 'in',
          body: '',
          at: DateTime.now(),
          status: ChatStatusValue.pendingApproval,
          kind: ChatMessageKind.file,
          fileName: 'theirs-$i.bin',
          fileSize: 7,
        ),
    ];
    await _pumpChats(tester, fake);

    expect(find.text('3 file offers need your attention'), findsOneWidget);
    expect(find.byIcon(Icons.move_to_inbox_rounded), findsOneWidget);
    expect(find.text('3'), findsOneWidget); // the Badge label
    expect(find.textContaining('unread'), findsNothing);
  });

  // A thread whose only traffic is text — however much of it — is not waiting
  // on the user for anything, and must not be badged as though it were.
  testWidgets('a thread full of text shows no attention badge at all', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      for (var i = 0; i < 5; i++) _text('pb-bob', 'm$i', body: 'message $i'),
    ];
    await _pumpChats(tester, fake);

    expect(find.textContaining('needs your attention'), findsNothing);
    expect(find.byIcon(Icons.move_to_inbox_rounded), findsNothing);
    expect(find.textContaining('Last message'), findsOneWidget);
  });

  // `last_timestamp` is NULLABLE: a thread whose rows this build cannot read is
  // still listed (dropping it would hide the very conversation this list exists
  // to make reachable), and it has nothing to say about when it last moved. It
  // must say exactly that, and never invent a time — "Last message just now" on
  // a thread nobody has touched in a year is a fabricated fact.
  testWidgets('a thread with no known timestamp invents no time', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    // A thread whose namespace exists but whose rows this build cannot decode:
    // the engine still lists it (from the namespace), and its history reads
    // back empty, so there is no timestamp to report.
    fake.chatHistories['pb-unreadable'] = [];
    await _pumpChats(tester, fake);

    expect(find.text('pb-unreadable'), findsOneWidget);
    expect(find.text('No messages to show'), findsOneWidget);
    expect(find.textContaining('Last message'), findsNothing);
    expect(find.textContaining('ago'), findsNothing);
    expect(find.textContaining('just now'), findsNothing);
  });

  testWidgets('no conversations shows an empty state, not a bare list', (
    tester,
  ) async {
    await _pumpChats(tester, FakePeerBeam());
    expect(find.text('No conversations yet'), findsOneWidget);
  });

  // A message from a peer this session has never seen creates the thread; the
  // list has to notice without the user leaving and coming back.
  testWidgets('an arriving message adds its conversation live', (tester) async {
    final fake = FakePeerBeam();
    await _pumpChats(tester, fake);
    expect(find.text('No conversations yet'), findsOneWidget);

    // The engine persists the record, then announces it.
    fake.chatHistories['pb-new'] = [_text('pb-new', 'm1')];
    fake.emit(ChatReceived(_text('pb-new', 'm1')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.text('pb-new'), findsOneWidget);
  });

  // ── deleting a conversation ──────────────────────────────────

  // THE TRAP, at the surface. "Delete the history, keep the queue" is the whole
  // semantic: a queued file's record must survive, because the engine's drain
  // reads a missing record as "nothing will ever settle this" and throws the
  // file away. So the thread that kept something stays listed — immediately,
  // rather than reappearing later out of nowhere — and the user is told what
  // was kept, in the engine's own numbers.
  testWidgets('deleting a thread with a queued file keeps it, and says so', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      _text('pb-bob', 'm1'),
      _text('pb-bob', 'm2'),
      _queuedFile('pb-bob', 'fr-1', 'movie.mkv'),
    ];
    fake.queuedMessageIds.add('fr-1');
    await _pumpChats(tester, fake);

    await tester.tap(find.byTooltip('Conversation options'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Delete conversation'));
    await tester.pumpAndSettle();

    // The confirmation names the peer and promises what will be kept, without
    // quoting a count it has not been given.
    expect(find.text('Delete "pb-bob"?'), findsOneWidget);
    expect(
      find.textContaining('still waiting to be sent is kept and will still be '
          'sent'),
      findsOneWidget,
    );

    await tester.tap(find.widgetWithText(FilledButton, 'Delete'));
    await tester.pumpAndSettle();

    expect(fake.calls, contains('chatDelete:pb-bob'));
    // The engine kept the queued record, so the thread is still there — and the
    // report is the engine's own count, singular because one was kept.
    expect(fake.chatHistories['pb-bob'], hasLength(1));
    expect(fake.chatHistories['pb-bob']!.single.id, 'fr-1');
    expect(
      find.text(
        'Deleted 2 messages from "pb-bob" · 1 queued message was kept and '
        'will still be sent',
      ),
      findsOneWidget,
    );
    expect(find.text('pb-bob'), findsOneWidget, reason: 'still listed');
  });

  // Nothing queued means nothing to protect: the thread goes completely, and
  // the row disappears rather than coming back empty.
  testWidgets('deleting a thread with nothing queued removes it entirely', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      for (var i = 0; i < 3; i++) _text('pb-bob', 'm$i'),
    ];
    await _pumpChats(tester, fake);

    await tester.tap(find.byTooltip('Conversation options'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Delete conversation'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, 'Delete'));
    await tester.pumpAndSettle();

    expect(fake.chatHistories.containsKey('pb-bob'), isFalse);
    // No queue, so no promise about one — the report says only what happened.
    expect(find.text('Deleted 3 messages from "pb-bob"'), findsOneWidget);
    expect(find.textContaining('will still be sent'), findsNothing);
    expect(find.text('No conversations yet'), findsOneWidget);
  });

  testWidgets('cancelling the confirmation deletes nothing', (tester) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [_text('pb-bob', 'm1')];
    await _pumpChats(tester, fake);

    await tester.tap(find.byTooltip('Conversation options'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Delete conversation'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(TextButton, 'Cancel'));
    await tester.pumpAndSettle();

    expect(fake.calls, isNot(contains('chatDelete:pb-bob')));
    expect(fake.chatHistories['pb-bob'], hasLength(1));
    expect(find.text('pb-bob'), findsOneWidget);
  });

  // A refusal is the engine declining to delete because it could not establish
  // what is still queued — the thread is still there, and saying nothing would
  // look exactly like success.
  testWidgets('a delete the engine refuses is reported, not swallowed', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [_text('pb-bob', 'm1')];
    fake.failChatDelete = true;
    await _pumpChats(tester, fake);

    await tester.tap(find.byTooltip('Conversation options'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Delete conversation'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, 'Delete'));
    await tester.pumpAndSettle();

    expect(find.textContaining('Could not delete "pb-bob"'), findsOneWidget);
    expect(fake.chatHistories['pb-bob'], hasLength(1));
    expect(find.text('pb-bob'), findsOneWidget);
  });
}
