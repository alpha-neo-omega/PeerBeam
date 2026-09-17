// Consent you can give and consent you can take back, and a composer that
// knows which of the two it is looking at.
//
// Invariant I6 requires auto-accept to be explicit, per-capability **and
// revocable**. Both defects here are that word: a stored consent that could
// only be given, and a permission the user had withdrawn that the screen
// carried on offering to use.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/chat/chat_screen.dart';
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

TrustedDevice _trusted({
  required bool approved,
  required Set<String> permissions,
  bool autoAccept = false,
}) => TrustedDevice(
  id: 'pb-bob',
  name: 'Bob',
  fingerprint: 'ab:cd',
  trustedAt: DateTime.utc(2026, 1, 1),
  approved: approved,
  permissions: permissions,
  autoAccept: autoAccept,
);

Future<AppState> _open(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await state.trust.refresh();
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
  return state;
}

void main() {
  group('a revoked Messages permission', () {
    // The engine refuses before persisting anything, so the message never
    // existed — the only answer was a red bubble printing the engine's own
    // sentence about a `chat` permission, inside a bubble capped at 75% of the
    // column. The composer had no business being live.
    testWidgets('closes the composer and says who turned it off', (
      tester,
    ) async {
      final fake = FakePeerBeam()
        ..trusted = [
          _trusted(approved: true, permissions: {PeerBeamPermission.files}),
        ];
      await _open(tester, fake);

      final field = tester.widget<TextField>(find.byType(TextField));
      expect(field.enabled, isFalse);
      expect(
        find.textContaining('turned off Messages for Bob'),
        findsOneWidget,
      );
      // And not blamed on the network, which is the other reason a composer
      // closes and needs a different sentence.
      expect(find.textContaining('No address known'), findsNothing);
    });

    testWidgets('a device that still has Messages can write', (tester) async {
      final fake = FakePeerBeam()
        ..trusted = [
          _trusted(approved: true, permissions: {PeerBeamPermission.chat}),
        ];
      await _open(tester, fake);

      expect(tester.widget<TextField>(find.byType(TextField)).enabled, isTrue);
    });

    // An unapproved or never-seen peer is a different situation: the thread is
    // worth opening and the first message is what prompts the approval, so it
    // must not be pre-emptively closed.
    testWidgets('an unapproved device is not treated as revoked', (
      tester,
    ) async {
      final fake = FakePeerBeam()
        ..trusted = [_trusted(approved: false, permissions: const {})];
      await _open(tester, fake);

      expect(tester.widget<TextField>(find.byType(TextField)).enabled, isTrue);
      expect(find.textContaining('turned off Messages'), findsNothing);
    });

    testWidgets('a device with no trust record at all can still write', (
      tester,
    ) async {
      await _open(tester, FakePeerBeam());
      expect(tester.widget<TextField>(find.byType(TextField)).enabled, isTrue);
    });
  });

  group('auto-accept can always be withdrawn', () {
    // The defect: `enabled: blocked == null` disabled the row whenever the
    // device could not be *granted* auto-accept — including when it already had
    // it and the user had since revoked its Files permission. The bit stayed
    // set behind a greyed-out row, and the day Files came back, that device's
    // files were saved without asking again. I6 says revocable.
    testWidgets('the off direction is offered even while Files is revoked', (
      tester,
    ) async {
      final fake = FakePeerBeam()
        ..trusted = [
          _trusted(
            approved: true,
            permissions: {PeerBeamPermission.chat},
            autoAccept: true,
          ),
        ];
      await _open(tester, fake);

      await tester.tap(find.byTooltip('More'));
      await tester.pumpAndSettle();

      expect(find.text('Ask about files again'), findsOneWidget);
      final item = tester.widget<PopupMenuItem<bool>>(
        find.ancestor(
          of: find.text('Ask about files again'),
          matching: find.byType(PopupMenuItem<bool>),
        ),
      );
      expect(item.enabled, isTrue, reason: 'consent must be withdrawable');
    });

    // Turning it ON is still gated — that is the direction the block is about.
    testWidgets('the on direction stays blocked, and says why', (tester) async {
      final fake = FakePeerBeam()
        ..trusted = [
          _trusted(approved: true, permissions: {PeerBeamPermission.chat}),
        ];
      await _open(tester, fake);

      await tester.tap(find.byTooltip('More'));
      await tester.pumpAndSettle();

      expect(find.text('Accept files without asking'), findsOneWidget);
      final item = tester.widget<PopupMenuItem<bool>>(
        find.ancestor(
          of: find.text('Accept files without asking'),
          matching: find.byType(PopupMenuItem<bool>),
        ),
      );
      expect(item.enabled, isFalse);
      expect(find.textContaining('may not send files'), findsOneWidget);
    });
  });
}
