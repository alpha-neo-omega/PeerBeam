// The chat bar's per-device "accept files without asking" control.
//
// Three things are load-bearing:
//   1. It writes the engine bit for THIS device and no other — the whole reason
//      it exists, since the global setting is all-or-nothing.
//   2. It is unavailable, with a stated reason, whenever the engine could not
//      honour it: an unpinned device, an unapproved one, or one whose `files`
//      permission is revoked. A switch that silently does nothing is worse than
//      no switch.
//   3. It reflects the engine's answer, not a local guess.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/chat/chat_screen.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

TrustedDevice _device({
  required String id,
  bool approved = true,
  bool autoAccept = false,
  Set<String>? permissions,
}) => TrustedDevice(
  id: id,
  name: 'Laptop',
  fingerprint: 'ff' * 32,
  trustedAt: DateTime.utc(2026, 8, 20),
  approved: approved,
  permissions:
      permissions ??
      (approved
          ? {'files', 'chat', 'clipboard', 'presence', 'pipe'}
          : const <String>{}),
  autoAccept: autoAccept,
);

Future<AppState> _open(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await state.trust.refresh();
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: MaterialApp(
        home: ChatScreen(
          peerId: 'pb-peer',
          peer: const PeerTarget(
            id: 'pb-peer',
            name: 'Laptop',
            addresses: ['10.0.0.5'],
            port: 51000,
          ),
        ),
      ),
    ),
  );
  await tester.pumpAndSettle();
  return state;
}

/// Open the chat bar's overflow menu.
Future<void> _openMenu(WidgetTester tester) async {
  await tester.tap(find.byIcon(Icons.more_vert_rounded));
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('turning it on asks the engine for this device only', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..trusted = [_device(id: 'pb-peer'), _device(id: 'pb-other')];
    await _open(tester, fake);
    await _openMenu(tester);

    expect(find.text('Accept files without asking'), findsOneWidget);
    await tester.tap(find.text('Accept files without asking'));
    await tester.pumpAndSettle();

    expect(fake.autoAcceptCalls, hasLength(1));
    expect(fake.autoAcceptCalls.single.id, 'pb-peer');
    expect(fake.autoAcceptCalls.single.autoAccept, isTrue);
    // The other device is untouched — the point of a per-device answer.
    expect(
      fake.trusted.firstWhere((d) => d.id == 'pb-other').autoAccept,
      isFalse,
    );
  });

  testWidgets('when it is already on, the entry offers to turn it off', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..trusted = [_device(id: 'pb-peer', autoAccept: true)];
    await _open(tester, fake);
    await _openMenu(tester);

    expect(find.text('Ask about files again'), findsOneWidget);
    await tester.tap(find.text('Ask about files again'));
    await tester.pumpAndSettle();

    expect(fake.autoAcceptCalls.single.autoAccept, isFalse);
  });

  testWidgets('an unapproved device is refused, and told why', (tester) async {
    final fake = FakePeerBeam()
      ..trusted = [_device(id: 'pb-peer', approved: false)];
    await _open(tester, fake);
    await _openMenu(tester);

    // Auto-accepting from a device that may send nothing is meaningless, and a
    // control that read "on" would promise what the engine will not do.
    expect(find.textContaining('Trust this device first'), findsOneWidget);
    await tester.tap(find.text('Accept files without asking'));
    await tester.pumpAndSettle();
    expect(fake.autoAcceptCalls, isEmpty, reason: 'the entry is disabled');
  });

  testWidgets('a device with files revoked is refused, and told why', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..trusted = [
        _device(id: 'pb-peer', permissions: {'chat', 'clipboard'}),
      ];
    await _open(tester, fake);
    await _openMenu(tester);

    expect(find.textContaining('may not send files'), findsOneWidget);
    expect(fake.autoAcceptCalls, isEmpty);
  });

  testWidgets('a device that has never connected is refused, and told why', (
    tester,
  ) async {
    final fake = FakePeerBeam()..trusted = const [];
    await _open(tester, fake);
    await _openMenu(tester);

    expect(find.textContaining('has not connected yet'), findsOneWidget);
    expect(fake.autoAcceptCalls, isEmpty);
  });
}
