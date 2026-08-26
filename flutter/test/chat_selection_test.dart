// Selecting messages inside a conversation, and the two things that can be
// done with a selection: forward them to another device, or delete this
// device's copy of them.
//
// Both semantics are settled and both are load-bearing here:
//
//  * selection is of MESSAGES INSIDE a thread, not of threads in the Chats
//    list — which is why every test below drives `ChatScreen`;
//  * delete means **delete for me**. Nothing goes on the wire, the peer keeps
//    its copy, and the engine refuses to take a row that still backs a queued
//    send. The surface's whole job is to report that refusal honestly instead
//    of claiming a message it could not delete is gone.

import 'dart:async';
import 'dart:io';

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'package:peerbeam/features/chat/chat_screen.dart';
import 'package:peerbeam/sdk/events.dart';
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

/// A discovered device to forward to — online and addressable, which is what
/// `showDevicePicker` requires before it will offer one.
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

ChatMessage _text(String id, String body, {String direction = 'out'}) =>
    ChatMessage(
      id: id,
      peerId: 'pb-bob',
      direction: direction,
      body: body,
      at: DateTime.now(),
      status: ChatStatusValue.sent,
    );

ChatMessage _file(
  String id,
  String name, {
  String? localPath,
  String status = ChatStatusValue.sent,
  String direction = 'out',
}) => ChatMessage(
  id: id,
  peerId: 'pb-bob',
  direction: direction,
  body: '',
  at: DateTime.now(),
  status: status,
  kind: ChatMessageKind.file,
  fileName: name,
  fileSize: 4,
  localPath: localPath,
);

/// A real file on disk, cleaned up with the test. Forwarding asks the
/// filesystem whether a row's bytes are still here, so the tests that care
/// about that answer give it real files to find (and real paths to miss).
String _realFile(String name) {
  final dir = Directory.systemTemp.createTempSync('pb-forward');
  addTearDown(() {
    if (dir.existsSync()) dir.deleteSync(recursive: true);
  });
  final f = File('${dir.path}/$name')..writeAsStringSync('bytes');
  return f.path;
}

Widget _screen(AppState state) => AppScope(
  state: state,
  child: const MaterialApp(
    home: ChatScreen(peerId: 'pb-bob', peer: _peer),
  ),
);

/// Pump the chat screen and let its post-frame `openThread` land.
Future<AppState> _open(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(_screen(state));
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 300));
  return state;
}

/// Enter selection the way a touch user does.
Future<void> _longPress(WidgetTester tester, String text) async {
  await tester.longPress(find.text(text));
  await tester.pumpAndSettle();
}

/// Forward the current selection to [deviceName] through the shared device
/// picker — the same sheet the Send flow opens.
Future<void> _forwardTo(WidgetTester tester, String deviceName) async {
  await tester.tap(find.byTooltip('Forward'));
  await tester.pumpAndSettle();
  await tester.tap(find.text(deviceName));
  await tester.pumpAndSettle();
}

/// Confirm the delete dialog.
Future<void> _confirmDelete(WidgetTester tester) async {
  await tester.tap(find.byTooltip('Delete'));
  await tester.pumpAndSettle();
  await tester.tap(find.widgetWithText(FilledButton, 'Delete'));
  await tester.pumpAndSettle();
}

void main() {
  // ── entering and leaving selection ────────────────────────────

  testWidgets('a long-press selects exactly that message; a tap toggles the '
      'next; the close action clears everything', (tester) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      _text('m1', 'first'),
      _text('m2', 'second'),
      _text('m3', 'third'),
    ];
    await _open(tester, fake);

    // Nothing is selected until the user says so — the ordinary app bar.
    expect(find.text('Bob'), findsOneWidget);

    await _longPress(tester, 'second');
    expect(
      find.text('1 selected'),
      findsOneWidget,
      reason: 'a long-press starts the selection with that one message',
    );

    // Once selecting, a plain tap adds…
    await tester.tap(find.text('third'));
    await tester.pumpAndSettle();
    expect(find.text('2 selected'), findsOneWidget);

    // …and taking one back removes it, down to leaving selection entirely.
    await tester.tap(find.text('third'));
    await tester.pumpAndSettle();
    expect(find.text('1 selected'), findsOneWidget);
    await tester.tap(find.text('second'));
    await tester.pumpAndSettle();
    expect(
      find.text('Bob'),
      findsOneWidget,
      reason: 'deselecting the last message leaves selection',
    );

    // And the × leaves from anywhere.
    await _longPress(tester, 'first');
    expect(find.text('1 selected'), findsOneWidget);
    await tester.tap(find.byTooltip('Cancel selection'));
    await tester.pumpAndSettle();
    expect(find.text('Bob'), findsOneWidget);
    expect(find.text('1 selected'), findsNothing);
  });

  // A right-click is the desktop way in; a long-press with a mouse is not.
  /// **You could not get a message's text out of the app.** The bubble is a
  /// plain `Text` — not selectable, because long-press and right-click are how
  /// selection mode is entered — so there was no way to copy what somebody sent
  /// you. Copy joins the actions that already work on a selection.
  testWidgets('copy puts the selected messages on the clipboard, in thread '
      'order', (tester) async {
    final copied = <String>[];
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      SystemChannels.platform,
      (call) async {
        if (call.method == 'Clipboard.setData') {
          copied.add((call.arguments as Map)['text'] as String);
        }
        return null;
      },
    );
    addTearDown(
      () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        SystemChannels.platform,
        null,
      ),
    );

    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      _text('m1', 'first'),
      _text('m2', 'second'),
    ];
    await _open(tester, fake);

    // Selected out of order; copied in the order they appear on screen.
    await _longPress(tester, 'second');
    await tester.tap(find.text('first'));
    await tester.pumpAndSettle();

    await tester.tap(find.byTooltip('Copy text'));
    await tester.pumpAndSettle();

    expect(copied, ['first\nsecond']);
    expect(
      find.text('2 selected'),
      findsNothing,
      reason: 'copying finishes the selection, like the other actions do',
    );
  });

  testWidgets('a secondary tap enters selection too', (tester) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [_text('m1', 'first')];
    await _open(tester, fake);

    final gesture = await tester.startGesture(
      tester.getCenter(find.text('first')),
      kind: PointerDeviceKind.mouse,
      buttons: kSecondaryMouseButton,
    );
    await gesture.up();
    await tester.pumpAndSettle();

    expect(find.text('1 selected'), findsOneWidget);
  });

  // Back must land on the selection first. A back press that closed the whole
  // conversation while a selection was open would throw away work the user is
  // in the middle of and move them somewhere they never asked to go.
  testWidgets('back closes the selection before it closes the screen', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [_text('m1', 'first')];
    final state = AppState.live(fake);
    addTearDown(state.dispose);

    // Pushed over a host route, so "closes the screen" is observable.
    await tester.pumpWidget(
      AppScope(
        state: state,
        child: const MaterialApp(home: Scaffold(body: Text('behind'))),
      ),
    );
    final nav = tester.state<NavigatorState>(find.byType(Navigator));
    unawaited(
      nav.push(
        MaterialPageRoute<void>(
          builder: (_) => const ChatScreen(peerId: 'pb-bob', peer: _peer),
        ),
      ),
    );
    await tester.pumpAndSettle();
    await tester.pump(const Duration(milliseconds: 300));

    await _longPress(tester, 'first');
    expect(find.text('1 selected'), findsOneWidget);

    // The first back press is spent on the selection.
    await nav.maybePop();
    await tester.pumpAndSettle();
    expect(find.text('1 selected'), findsNothing);
    expect(
      find.text('Bob'),
      findsOneWidget,
      reason: 'the conversation is still open',
    );
    expect(find.text('behind'), findsNothing);

    // Only the second one leaves the conversation.
    await nav.maybePop();
    await tester.pumpAndSettle();
    expect(find.text('behind'), findsOneWidget);
  });

  // Engine events keep arriving while a selection sits open — that is the
  // ordinary state of a chat screen, not an edge case. A rebuild driven by an
  // incoming message must not quietly drop what the user has picked.
  testWidgets('a selection survives an unrelated incoming message', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      _text('m1', 'first'),
      _text('m2', 'second'),
    ];
    await _open(tester, fake);

    await _longPress(tester, 'first');
    await tester.tap(find.text('second'));
    await tester.pumpAndSettle();
    expect(find.text('2 selected'), findsOneWidget);

    fake.emit(ChatReceived(_text('m9', 'and hello from Bob', direction: 'in')));
    await tester.pumpAndSettle();

    expect(find.text('and hello from Bob'), findsOneWidget);
    expect(
      find.text('2 selected'),
      findsOneWidget,
      reason: 'the arrival is not a reason to forget what was selected',
    );
  });

  // AN ID THE PEER CHOOSES. `build` narrows the selection for rendering, which
  // is not the same as pruning it — a partially stale set keeps naming its dead
  // ids because the live ones carried it. And an inbound file's message id is
  // the SENDER's `FileRef` id, a value the peer picks, so a peer that reuses a
  // dead id gets a message the user never chose rendered as already selected,
  // counted, and forwarded to a third device.
  testWidgets('an id that leaves the thread is pruned, so a peer reusing it '
      'inherits no selection', (tester) async {
    final path = _realFile('from-bob.pdf');
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      _text('m1', 'first'),
      _text('m2', 'second'),
    ];
    final state = await _open(tester, fake);
    fake.emit(const DeviceAdded(_laptop));
    await tester.pumpAndSettle();

    await _longPress(tester, 'first');
    await tester.tap(find.text('second'));
    await tester.pumpAndSettle();
    expect(find.text('2 selected'), findsOneWidget);

    // `m1` goes from another surface and the thread is re-read. `m2` survives,
    // so the selection is only PARTIALLY stale — the case a rule that only
    // clears an entirely-dead set walks straight past.
    fake.chatHistories['pb-bob'] = [_text('m2', 'second')];
    await state.chat.refresh('pb-bob');
    await tester.pumpAndSettle();
    expect(find.text('1 selected'), findsOneWidget);

    // Bob then offers a file whose `FileRef` id happens to be `m1`.
    fake.emit(
      ChatReceived(
        _file(
          'm1',
          'from-bob.pdf',
          localPath: path,
          direction: 'in',
          status: ChatStatusValue.received,
        ),
      ),
    );
    await tester.pumpAndSettle();

    expect(find.text('from-bob.pdf'), findsOneWidget);
    expect(
      find.text('1 selected'),
      findsOneWidget,
      reason: 'the arrival is a different message, not one the user picked',
    );
    expect(
      find.byIcon(Icons.check_circle_rounded),
      findsOneWidget,
      reason: 'only `second` is ticked',
    );
    expect(find.byIcon(Icons.radio_button_unchecked_rounded), findsOneWidget);

    // And the consequence that actually costs something: a Forward must not
    // carry Bob's file to a third device off the back of a recycled id.
    await _forwardTo(tester, 'Live Laptop');
    expect(
      fake.calls
          .where(
            (c) => c.startsWith('chatSend') || c.startsWith('chatSendFile'),
          )
          .toList(),
      ['chatSend:second'],
    );
  });

  // ── delete ────────────────────────────────────────────────────

  testWidgets('delete asks first, then calls the engine once with exactly the '
      'selected ids', (tester) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      _text('m1', 'first'),
      _text('m2', 'second'),
      _text('m3', 'third'),
    ];
    await _open(tester, fake);

    await _longPress(tester, 'first');
    await tester.tap(find.text('third'));
    await tester.pumpAndSettle();

    await tester.tap(find.byTooltip('Delete'));
    await tester.pumpAndSettle();
    // The confirmation says what it can honestly promise before the fact, and
    // quotes no counts of what will survive.
    expect(find.text('Delete 2 messages?'), findsOneWidget);
    expect(
      find.textContaining(
        'still waiting to be sent is kept and will still be '
        'sent',
      ),
      findsOneWidget,
    );
    expect(
      find.textContaining('The other device keeps its own copy'),
      findsOneWidget,
    );

    await tester.tap(find.widgetWithText(FilledButton, 'Delete'));
    await tester.pumpAndSettle();

    expect(
      fake.calls.where((c) => c.startsWith('chatDeleteMessages')).toList(),
      ['chatDeleteMessages:pb-bob/m1,m3'],
      reason: 'one call, the whole selection, and nothing it was not given',
    );
    expect(find.text('Deleted 2 messages'), findsOneWidget);
    // Selection is over, and the untouched message is still there.
    expect(find.text('Bob'), findsOneWidget);
    expect(find.text('second'), findsOneWidget);
    expect(find.text('first'), findsNothing);
  });

  testWidgets('cancelling the confirmation deletes nothing', (tester) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [_text('m1', 'first')];
    await _open(tester, fake);

    await _longPress(tester, 'first');
    await tester.tap(find.byTooltip('Delete'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(TextButton, 'Cancel'));
    await tester.pumpAndSettle();

    expect(fake.calls.any((c) => c.startsWith('chatDeleteMessages')), isFalse);
    expect(find.text('first'), findsOneWidget);
    expect(find.text('1 selected'), findsOneWidget, reason: 'still selected');
  });

  // THE POINT OF THE ENGINE RETURNING `kept`. A row that still backs a queued
  // send is refused, because deleting it is what makes the drain conclude
  // nothing will ever settle the entry — it then drops the entry and deletes
  // the file's only staged copy. So the report is the engine's own answer, and
  // it says why the bubble the user picked is still on screen.
  testWidgets('the report is the engine\'s removed/kept, not the request', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      _text('m1', 'first'),
      _text('m2', 'second'),
      _file('fr-1', 'movie.mkv', status: ChatStatusValue.pending),
    ];
    fake.queuedMessageIds.add('fr-1'); // still waiting for Bob

    await _open(tester, fake);
    await _longPress(tester, 'first');
    await tester.tap(find.text('second'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('movie.mkv'));
    await tester.pumpAndSettle();
    expect(find.text('3 selected'), findsOneWidget);

    await _confirmDelete(tester);

    expect(
      find.text('Deleted 2 messages · 1 kept because it is still being sent'),
      findsOneWidget,
    );
    expect(
      find.text('movie.mkv'),
      findsOneWidget,
      reason: 'the kept row stays exactly where it was',
    );
    expect(find.text('first'), findsNothing);
  });

  testWidgets('a delete the engine refuses is reported, not swallowed', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [_text('m1', 'first')];
    fake.failChatDelete = true;
    await _open(tester, fake);

    await _longPress(tester, 'first');
    await _confirmDelete(tester);

    expect(find.textContaining('Could not delete'), findsOneWidget);
    expect(
      find.text('first'),
      findsOneWidget,
      reason: 'nothing was deleted, so nothing may disappear',
    );
  });

  // ── forward ───────────────────────────────────────────────────

  // Thread order, and each kind through its own send path. Asserting the CALLS
  // rather than a count is the whole point: forwarding a conversation whose
  // messages arrive shuffled, or a file that arrives as a line of text, is a
  // different conversation.
  testWidgets('forward sends every selected message to the picked device, in '
      'thread order, text as text and file as file', (tester) async {
    final path = _realFile('report.pdf');
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      _text('m1', 'first'),
      _file('fr-1', 'report.pdf', localPath: path),
      _text('m2', 'second'),
    ];
    await _open(tester, fake);
    fake.emit(const DeviceAdded(_laptop));
    await tester.pumpAndSettle();

    // Selected out of order on purpose — the send order is the thread's.
    await _longPress(tester, 'second');
    await tester.tap(find.text('report.pdf'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('first'));
    await tester.pumpAndSettle();
    expect(find.text('3 selected'), findsOneWidget);

    await _forwardTo(tester, 'Live Laptop');

    expect(
      fake.calls
          .where(
            (c) => c.startsWith('chatSend') || c.startsWith('chatSendFile'),
          )
          .toList(),
      ['chatSend:first', 'chatSendFile:$path', 'chatSend:second'],
    );
    // The rows landed in the OTHER conversation; this one is untouched.
    expect(fake.chatHistories['x1'], hasLength(3));
    expect(fake.chatHistories['pb-bob'], hasLength(3));
    expect(find.text('Forwarded 3 messages to Live Laptop'), findsOneWidget);
    expect(find.text('Bob'), findsOneWidget, reason: 'selection is over');
  });

  // A file whose bytes have gone cannot be forwarded at all, and the engine
  // must never be handed the path to find that out one message at a time.
  testWidgets('a file whose bytes are gone is excluded and named; the rest '
      'still go', (tester) async {
    final path = _realFile('here.pdf');
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      _file('fr-1', 'here.pdf', localPath: path),
      _file('fr-2', 'invoice.pdf', localPath: '${path}_moved_away'),
      _text('m1', 'and some text'),
    ];
    await _open(tester, fake);
    fake.emit(const DeviceAdded(_laptop));
    await tester.pumpAndSettle();

    await _longPress(tester, 'here.pdf');
    await tester.tap(find.text('invoice.pdf'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('and some text'));
    await tester.pumpAndSettle();

    await _forwardTo(tester, 'Live Laptop');

    expect(
      fake.calls
          .where(
            (c) => c.startsWith('chatSend') || c.startsWith('chatSendFile'),
          )
          .toList(),
      ['chatSendFile:$path', 'chatSend:and some text'],
      reason: 'the missing file is never offered to the engine',
    );
    expect(
      find.text(
        "Forwarded 2 messages to Live Laptop · invoice.pdf isn't on this "
        'device any more',
      ),
      findsOneWidget,
    );
  });

  testWidgets('a selection of only-missing files sends nothing and says so', (
    tester,
  ) async {
    final path = _realFile('anchor.txt');
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      _file('fr-1', 'invoice.pdf', localPath: '${path}_gone'),
    ];
    await _open(tester, fake);
    fake.emit(const DeviceAdded(_laptop));
    await tester.pumpAndSettle();

    await _longPress(tester, 'invoice.pdf');
    await _forwardTo(tester, 'Live Laptop');

    expect(
      fake.calls.any(
        (c) => c.startsWith('chatSend') || c.startsWith('chatSendFile'),
      ),
      isFalse,
    );
    expect(
      find.text(
        "Nothing could be forwarded — invoice.pdf isn't on this device any "
        'more',
      ),
      findsOneWidget,
    );
    expect(
      find.text('1 selected'),
      findsOneWidget,
      reason:
          'nothing was sent, so the selection is still the user\'s to act on',
    );
  });

  // FORWARDING TO A SAVED PICK. The device picker offers saved (by-address)
  // entries alongside discovered ones, and a saved entry's id is a locally
  // minted timestamp the peer has never heard of. Filing forwarded rows under
  // it would create a thread every inbound record misses — replies are keyed by
  // the authenticated device id — so the pick is resolved back to a discovered
  // identity by the address it advertises, the same rule the Home screen
  // applies before it will open a conversation at all.
  testWidgets('a Saved pick is forwarded under the DISCOVERED device id, never '
      'the saved entry\'s synthetic one', (tester) async {
    SharedPreferences.setMockInitialValues({});
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [_text('m1', 'first')];
    final state = await _open(tester, fake);
    final saved = await state.saved.add(
      name: 'Server',
      host: '127.0.0.1',
      port: 49600,
    );
    // The same box turns up on discovery, at the saved address.
    fake.emit(const DeviceAdded(_laptop));
    await tester.pumpAndSettle();

    await _longPress(tester, 'first');
    await _forwardTo(tester, 'Server');

    expect(fake.calls.where((c) => c.startsWith('chatSend')).toList(), [
      'chatSend:first',
    ]);
    expect(
      fake.chatHistories['x1'],
      hasLength(1),
      reason: 'filed under the id the peer will actually write back into',
    );
    expect(
      fake.chatHistories.containsKey(saved.id),
      isFalse,
      reason: 'and never under the locally minted one',
    );
    expect(find.text('Forwarded 1 message to Server'), findsOneWidget);
  });

  testWidgets('a Saved pick discovery cannot see is refused, and nothing is '
      'forwarded', (tester) async {
    SharedPreferences.setMockInitialValues({});
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [_text('m1', 'first')];
    final state = await _open(tester, fake);
    // No discovered device at this address — nothing here can name a real
    // conversation, and guessing one is what the resolution exists to refuse.
    await state.saved.add(name: 'Headless Box', host: '10.0.0.5', port: 49600);
    await tester.pumpAndSettle();

    await _longPress(tester, 'first');
    await _forwardTo(tester, 'Headless Box');

    expect(fake.calls.any((c) => c.startsWith('chatSend')), isFalse);
    expect(
      find.textContaining('Cannot forward to Headless Box yet'),
      findsOneWidget,
    );
    expect(
      find.text('1 selected'),
      findsOneWidget,
      reason:
          'nothing was sent, so the selection is still the user\'s to act '
          'on',
    );
  });

  testWidgets('dismissing the device picker forwards nothing and keeps the '
      'selection', (tester) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [_text('m1', 'first')];
    await _open(tester, fake);
    fake.emit(const DeviceAdded(_laptop));
    await tester.pumpAndSettle();

    await _longPress(tester, 'first');
    await tester.tap(find.byTooltip('Forward'));
    await tester.pumpAndSettle();
    await tester.tapAt(const Offset(10, 10)); // outside the sheet
    await tester.pumpAndSettle();

    expect(fake.calls.any((c) => c.startsWith('chatSend')), isFalse);
    expect(find.text('1 selected'), findsOneWidget);
  });
}
