// An open thread must follow discovery, not the snapshot it was pushed with.
//
// `ChatScreen.peer` is frozen at push time, and the Conversations list pushes
// an address-less placeholder for any peer discovery cannot currently see —
// which is precisely the thread this screen exists to keep reachable. Read
// once, that placeholder never expires: the device turns up on Home, the engine
// can reach it again, and the composer, the attach button and the drop zone all
// stay dead until the user backs out and comes in again, with nothing on screen
// suggesting they should. "Automatic reconnect" has to mean something on the
// one surface that is open before the peer is.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/chat/chat_screen.dart';
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

/// Some other device entirely — discovery seeing *a* peer is not discovery
/// seeing *this* one.
const _phone = SdkDevice(
  id: 'zz',
  name: 'Someone Else',
  kind: 'phone',
  platform: 'android',
  addresses: ['127.0.0.2'],
  port: 49601,
  online: true,
  latencyMs: 9,
  reachableLan: true,
  reachableRemote: false,
);

/// The thread exactly as the Conversations list opens it for a peer discovery
/// cannot see: the engine's own conversation key, and a target with nowhere to
/// send.
const _placeholder = PeerTarget(id: 'x1', name: 'x1', addresses: [], port: 0);

Future<AppState> _open(
  WidgetTester tester,
  FakePeerBeam fake,
  PeerTarget peer,
) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: MaterialApp(
        home: ChatScreen(peerId: 'x1', peer: peer),
      ),
    ),
  );
  // Fixed pumps rather than `pumpAndSettle`: the app state keeps timers
  // running, so "settled" never arrives.
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 300));
  return state;
}

/// Let an emitted engine event reach the repositories and the frame after it.
Future<void> _settle(WidgetTester tester) async {
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 300));
}

bool _composerEnabled(WidgetTester tester) =>
    tester.widget<TextField>(find.byType(TextField)).enabled ?? true;

bool _sendEnabled(WidgetTester tester) =>
    tester
        .widget<IconButton>(find.widgetWithIcon(IconButton, Icons.send_rounded))
        .onPressed !=
    null;

bool _attachEnabled(WidgetTester tester) =>
    tester
        .widget<IconButton>(
          find.widgetWithIcon(IconButton, Icons.attach_file_rounded),
        )
        .onPressed !=
    null;

void main() {
  // The failure this file exists to prevent, end to end.
  testWidgets('a peer discovered while the thread is open becomes sendable in '
      'place', (tester) async {
    final fake = FakePeerBeam();
    await _open(tester, fake, _placeholder);

    // The state the Conversations list opens this thread in: readable, and
    // honest about not being sendable.
    expect(_composerEnabled(tester), isFalse);
    expect(_sendEnabled(tester), isFalse);
    expect(_attachEnabled(tester), isFalse);
    expect(find.textContaining('No address known for x1'), findsOneWidget);

    // The device comes back — the event Home would render a tile from.
    fake.emit(const DeviceAdded(_laptop));
    await _settle(tester);

    expect(
      find.textContaining('No address known'),
      findsNothing,
      reason: 'the screen went on saying the peer is unreachable',
    );
    expect(_composerEnabled(tester), isTrue);
    expect(_sendEnabled(tester), isTrue);
    expect(_attachEnabled(tester), isTrue);
    // The bar names the device discovery is advertising, not the raw id the
    // thread was opened under.
    expect(find.text('Live Laptop'), findsOneWidget);

    await tester.enterText(find.byType(TextField), 'are you back?');
    await tester.tap(find.widgetWithIcon(IconButton, Icons.send_rounded));
    await _settle(tester);

    expect(fake.calls, contains('chatSend:are you back?'));
    // Aimed at the address discovery has just supplied — which the target this
    // screen was pushed with never had, so this can only have come from a
    // re-resolved one.
    expect(fake.chatSendTargets.single.addresses, ['127.0.0.1']);
    expect(fake.chatSendTargets.single.port, 49600);
  });

  // Discovery wins even when the pushed target has an address of its own: it is
  // the one that knows where the peer is *now*. A device that moved networks
  // while its thread sat open would otherwise have every message sent to the
  // address it used to be at.
  testWidgets('a stale pushed address is superseded by the discovered one', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    await _open(
      tester,
      fake,
      const PeerTarget(
        id: 'x1',
        name: 'Laptop (old)',
        addresses: ['10.0.0.9'],
        port: 1234,
      ),
    );
    expect(find.text('Laptop (old)'), findsOneWidget);

    fake.emit(const DeviceAdded(_laptop));
    await _settle(tester);

    expect(find.text('Live Laptop'), findsOneWidget);

    await tester.enterText(find.byType(TextField), 'still here?');
    await tester.tap(find.widgetWithIcon(IconButton, Icons.send_rounded));
    await _settle(tester);

    expect(fake.chatSendTargets.single.addresses, ['127.0.0.1']);
    expect(fake.chatSendTargets.single.port, 49600);
  });

  // The fallback is not "any device will do". Resolution is by the peer id the
  // conversation is keyed on, and a thread whose own peer is still missing must
  // keep saying so rather than borrowing a stranger's address.
  testWidgets('another device appearing does not make this thread sendable', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    await _open(tester, fake, _placeholder);

    fake.emit(const DeviceAdded(_phone));
    await _settle(tester);

    expect(_composerEnabled(tester), isFalse);
    expect(_sendEnabled(tester), isFalse);
    expect(find.textContaining('No address known for x1'), findsOneWidget);
    expect(find.text('Someone Else'), findsNothing);
  });
}
