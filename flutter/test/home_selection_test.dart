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
}
