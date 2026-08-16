import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/features/home/home_screen.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/staging.dart';
import 'package:peerbeam/state/stores.dart';
import 'package:shared_preferences/shared_preferences.dart';
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

/// The [IconButton] carrying [tooltip] (which an IconButton renders as a
/// descendant `Tooltip`, so `byTooltip` alone finds the wrong widget).
IconButton _button(WidgetTester tester, String tooltip) => tester.widget(
  find.ancestor(
    of: find.byTooltip(tooltip),
    matching: find.byType(IconButton),
  ),
);

void main() {
  testWidgets('persistent selection bar appears when the stack is non-empty', (
    tester,
  ) async {
    final state = AppState.live(FakePeerBeam());
    await tester.pumpWidget(
      AppScope(state: state, child: const MaterialApp(home: HomeScreen())),
    );
    await tester.pump();

    // Empty stack → no bar.
    expect(find.textContaining('item'), findsNothing);

    state.staging.add([
      StagedFile(path: '/x/a.bin', name: 'a.bin', size: 5),
    ]);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 200)); // AnimatedSize

    // Non-empty stack → the bar shows the count.
    expect(find.textContaining('1 item'), findsOneWidget);
  });

  // A conversation is local history, so it must stay openable when the peer
  // drops offline — otherwise the thread (and, in the next increment, its
  // queue) is unreachable exactly when it matters.
  testWidgets('a device that goes offline stays listed with chat still '
      'available, while send is disabled', (tester) async {
    final fake = FakePeerBeam();
    final state = AppState.live(fake);
    addTearDown(state.dispose);
    await tester.pumpWidget(
      AppScope(state: state, child: const MaterialApp(home: HomeScreen())),
    );
    await tester.pump();

    fake.emit(const DeviceAdded(_laptop));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    expect(find.text('Live Laptop'), findsOneWidget);

    fake.emit(const DeviceStatusChanged('x1', false));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    // Still listed, and still has a way into the conversation.
    expect(find.text('Live Laptop'), findsOneWidget);
    expect(_button(tester, 'Chat with Live Laptop').onPressed, isNotNull);
    // Sending, which really does need the peer, stays disabled.
    expect(_button(tester, 'Send to Live Laptop').onPressed, isNull);
  });

  // A saved device's id is a locally minted timestamp, NOT the peer's real
  // device id. Opening a thread under it would namespace our own rows under an
  // id the peer never uses, so replies (keyed by the authenticated device id)
  // land in a different conversation and queued text can never flush. So there
  // is no chat action at all unless the peer is actually discovered — and then
  // it must route with the DISCOVERED id.
  testWidgets('a saved device that is not discovered offers no chat action', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final state = AppState.live(FakePeerBeam());
    addTearDown(state.dispose);
    await state.saved.add(name: 'Server', host: '10.0.0.5', port: 49600);
    await tester.pumpWidget(
      AppScope(state: state, child: const MaterialApp(home: HomeScreen())),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.text('Server'), findsOneWidget);
    expect(find.byTooltip('Chat with Server'), findsNothing);
  });

  testWidgets('a saved device that IS discovered chats under the discovered '
      'device\'s real id, never the saved entry\'s synthetic one', (
    tester,
  ) async {
    SharedPreferences.setMockInitialValues({});
    final fake = FakePeerBeam();
    final state = AppState.live(fake);
    addTearDown(state.dispose);
    final saved = await state.saved.add(
      name: 'Server',
      host: '127.0.0.1',
      port: 49600,
    );
    await tester.pumpWidget(
      AppScope(state: state, child: const MaterialApp(home: HomeScreen())),
    );
    await tester.pump();

    // The same box turns up on discovery, at the saved address.
    fake.emit(const DeviceAdded(_laptop));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    await tester.tap(find.byTooltip('Chat with Server'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    // The thread it opened is the one the peer will actually write into.
    expect(fake.calls, contains('chatHistory:x1'));
    expect(fake.calls, isNot(contains('chatHistory:${saved.id}')));
  });

  // ── 2b: the Conversations list ──────────────────────────────
  //
  // Without it, a thread is only reachable through a device tile — so a file
  // queued for a peer that has since gone offline has no entry point at all,
  // which is exactly the case 2b exists to serve.
  testWidgets('a conversation with a peer discovery cannot see is listed, and '
      'opens under the engine\'s own peer id', (tester) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-ghost'] = [
      ChatMessage(
        id: 'fr-1',
        peerId: 'pb-ghost',
        direction: 'out',
        body: '',
        at: DateTime.now(),
        status: ChatStatusValue.pending,
        kind: ChatMessageKind.file,
        fileName: 'movie.mkv',
        fileSize: 7,
      ),
    ];
    final state = AppState.live(fake);
    addTearDown(state.dispose);
    await tester.pumpWidget(
      AppScope(state: state, child: const MaterialApp(home: HomeScreen())),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.text('Conversations'), findsOneWidget);
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
    fake.chatHistories['x1'] = [
      ChatMessage(
        id: 'm1',
        peerId: 'x1',
        direction: 'in',
        body: 'hi',
        at: DateTime.now(),
        status: ChatStatusValue.received,
      ),
    ];
    final state = AppState.live(fake);
    addTearDown(state.dispose);
    await tester.pumpWidget(
      AppScope(state: state, child: const MaterialApp(home: HomeScreen())),
    );
    await tester.pump();
    fake.emit(const DeviceAdded(_laptop));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    // The row carries the live name, not the raw device id. (Counted with
    // `findsWidgets`, not an exact number: the device tile below renders the
    // same name, and whether it is laid out at all depends on how much else
    // is on screen.)
    expect(find.text('Live Laptop'), findsWidgets);
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
    final state = AppState.live(fake);
    addTearDown(state.dispose);
    await tester.pumpWidget(
      AppScope(state: state, child: const MaterialApp(home: HomeScreen())),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.text('1 file offer needs your attention'), findsOneWidget);
    expect(find.textContaining('unread'), findsNothing);
    expect(find.byTooltip('1 file offer is waiting for your decision'),
        findsOneWidget);
  });

  // A thread whose only traffic is text — however much of it — is not waiting
  // on the user for anything, and must not be badged as though it were.
  testWidgets('a thread full of text shows no attention badge at all', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      for (var i = 0; i < 5; i++)
        ChatMessage(
          id: 'm$i',
          peerId: 'pb-bob',
          direction: 'in',
          body: 'message $i',
          at: DateTime.now(),
          status: ChatStatusValue.received,
        ),
    ];
    final state = AppState.live(fake);
    addTearDown(state.dispose);
    await tester.pumpWidget(
      AppScope(state: state, child: const MaterialApp(home: HomeScreen())),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.textContaining('needs your attention'), findsNothing);
    expect(find.byIcon(Icons.move_to_inbox_rounded), findsNothing);
    expect(find.textContaining('Last message'), findsOneWidget);
  });

  testWidgets('no conversations means no section at all', (tester) async {
    final state = AppState.live(FakePeerBeam());
    addTearDown(state.dispose);
    await tester.pumpWidget(
      AppScope(state: state, child: const MaterialApp(home: HomeScreen())),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.text('Conversations'), findsNothing);
  });

  // A message from a peer this session has never seen creates the thread; the
  // list has to notice without the user leaving and coming back.
  testWidgets('an arriving message adds its conversation live', (tester) async {
    final fake = FakePeerBeam();
    final state = AppState.live(fake);
    addTearDown(state.dispose);
    await tester.pumpWidget(
      AppScope(state: state, child: const MaterialApp(home: HomeScreen())),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));
    expect(find.text('Conversations'), findsNothing);

    // The engine persists the record, then announces it.
    fake.chatHistories['pb-new'] = [
      ChatMessage(
        id: 'm1',
        peerId: 'pb-new',
        direction: 'in',
        body: 'hello',
        at: DateTime.now(),
        status: ChatStatusValue.received,
      ),
    ];
    fake.emit(
      ChatReceived(
        ChatMessage(
          id: 'm1',
          peerId: 'pb-new',
          direction: 'in',
          body: 'hello',
          at: DateTime.now(),
          status: ChatStatusValue.received,
        ),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.text('Conversations'), findsOneWidget);
    expect(find.text('pb-new'), findsOneWidget);
  });
}
