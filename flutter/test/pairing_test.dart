// First-contact verification in the app: what an approval prompt shows when
// the sending device has never connected before, and what it takes to accept
// one.
//
// The engine owns the gate — it refuses an unconfirmed first-contact accept
// whoever asks, and it un-pins a refused peer — and its own tests prove that.
// What these tests hold down is the half only a user can see:
//
//   1. A first-contact prompt SAYS the device is new and shows the code, in
//      full. A known device's prompt shows neither and is untouched.
//   2. With the check on, no accept reaches the engine until the user has
//      explicitly confirmed. Dismissing the dialog is not confirming, and the
//      transfer survives it.
//   3. With the check off — the shipped default — a first-contact transfer
//      accepts in exactly the taps it always did.
//   4. Declining is never gated, and the toggle round-trips.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/settings/settings_screen.dart';
import 'package:peerbeam/features/transfers/transfers_screen.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/models.dart';
import 'package:peerbeam/state/stores.dart';
import 'package:peerbeam/widgets/pairing.dart';

import 'sdk/fake_peerbeam.dart';

/// A code shaped exactly as the engine emits one: eight groups of four
/// uppercase hex digits, 39 characters with the separators.
const _code = 'A1B2 C3D4 E5F6 0718 293A 4B5C 6D7E 8F90';

/// An inbound `transfer_queued`. [newlyTrusted] and [pairingCode] are the two
/// handshake facts the engine puts on this event and nowhere else.
TransferEvent _queued(
  String id, {
  bool newlyTrusted = false,
  String pairingCode = '',
}) => TransferEvent(
  kind: 'transfer_queued',
  transferId: id,
  timestamp: '',
  payload: {
    'peer': 'Bob',
    'file': '$id.bin',
    'incoming': true,
    'newly_trusted': newlyTrusted,
    'pairing_code': pairingCode,
  },
);

Future<AppState> _pumpTransfers(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: const MaterialApp(home: TransfersScreen()),
    ),
  );
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 600));
  return state;
}

/// Only the approval decisions the engine was actually asked to make. The
/// fake records a confirmed accept as `accept:<id>:confirmed`, so a test can
/// tell an accept that carried the user's answer from one that did not.
List<String> _decisions(FakePeerBeam fake) => fake.calls
    .where(
      (c) =>
          c.startsWith('accept:') ||
          c.startsWith('acceptTrust:') ||
          c.startsWith('reject:'),
    )
    .toList();

/// Emit an inbound `transfer_queued` and let the card land. The event travels a
/// broadcast stream and the card has an entrance animation, so a single pump
/// would assert against a frame the row is not on yet.
Future<void> _emit(
  WidgetTester tester,
  FakePeerBeam fake, {
  String id = 'tx-new',
  bool newlyTrusted = false,
  String pairingCode = '',
}) async {
  fake.emit(_queued(id, newlyTrusted: newlyTrusted, pairingCode: pairingCode));
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 600));
}

Future<void> _tap(WidgetTester tester, Finder f) async {
  await tester.tap(f);
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 300));
}

void main() {
  group('the first-contact prompt', () {
    testWidgets('says the device is new and shows the code in full', (
      tester,
    ) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);
      await _emit(tester, fake, newlyTrusted: true, pairingCode: _code);

      expect(find.text(firstContactTitle), findsOneWidget);
      // The whole code, character for character. A truncated safety number is
      // a grindable one, so nothing here may abbreviate it.
      expect(find.text(_code), findsOneWidget);
      expect(find.text(pairingCodeInstruction), findsOneWidget);
    });

    testWidgets('a known device is left exactly as it was', (tester) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);
      await _emit(tester, fake, id: 'tx-known');

      expect(find.text(firstContactTitle), findsNothing);
      expect(find.byType(PairingCodePanel), findsNothing);
      // And its Accept is still one tap, unchanged.
      await _tap(tester, find.text('Accept'));
      expect(_decisions(fake), ['accept:tx-known']);
    });

    testWidgets('the code is shown even with the check off — knowing a device '
        'is new is worth having by default', (tester) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);
      await _emit(tester, fake, newlyTrusted: true, pairingCode: _code);

      expect(find.text(_code), findsOneWidget);
    });
  });

  group('with the check ON', () {
    testWidgets('accept asks first, and only a confirmation gets through', (
      tester,
    ) async {
      final fake = FakePeerBeam();
      final state = await _pumpTransfers(tester, fake);
      state.settings.setRequirePairingConfirmation(true);
      await _emit(tester, fake, newlyTrusted: true, pairingCode: _code);

      await _tap(tester, find.text('Accept'));
      // Nothing has been accepted yet: the dialog is the whole answer so far.
      expect(_decisions(fake), isEmpty);
      expect(find.text('The codes match'), findsOneWidget);
      // The code is in the dialog too — the user is comparing it *here*.
      expect(find.text(_code), findsWidgets);

      await _tap(tester, find.text('The codes match'));
      expect(_decisions(fake), ['accept:tx-new:confirmed']);
    });

    testWidgets('cancelling confirms nothing, and the transfer survives it', (
      tester,
    ) async {
      final fake = FakePeerBeam();
      final state = await _pumpTransfers(tester, fake);
      state.settings.setRequirePairingConfirmation(true);
      await _emit(tester, fake, newlyTrusted: true, pairingCode: _code);

      await _tap(tester, find.text('Accept'));
      await _tap(tester, find.text('Cancel'));

      // Not accepted — and, just as importantly, not declined either. Being
      // asked to verify a device must not cost the user the file.
      expect(_decisions(fake), isEmpty);
      expect(find.text('Accept'), findsOneWidget);
      expect(find.text(firstContactTitle), findsOneWidget);
    });

    testWidgets('dismissing the dialog is not confirming', (tester) async {
      final fake = FakePeerBeam();
      final state = await _pumpTransfers(tester, fake);
      state.settings.setRequirePairingConfirmation(true);
      await _emit(tester, fake, newlyTrusted: true, pairingCode: _code);

      await _tap(tester, find.text('Accept'));
      // Popped from under it, the way a back gesture or a barrier tap does.
      // `showDialog` completes with null, which is not an answer.
      Navigator.of(tester.element(find.text('The codes match'))).pop();
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));

      expect(_decisions(fake), isEmpty);
    });

    testWidgets('Trust is gated too — it grants more, not less', (
      tester,
    ) async {
      final fake = FakePeerBeam();
      final state = await _pumpTransfers(tester, fake);
      state.settings.setRequirePairingConfirmation(true);
      await _emit(tester, fake, newlyTrusted: true, pairingCode: _code);

      await _tap(tester, find.text('Trust'));
      expect(_decisions(fake), isEmpty);
      await _tap(tester, find.text('The codes match'));
      expect(_decisions(fake), ['acceptTrust:tx-new:confirmed']);
    });

    testWidgets('declining is never gated — it is the answer someone who '
        'cannot match the codes needs most', (tester) async {
      final fake = FakePeerBeam();
      final state = await _pumpTransfers(tester, fake);
      state.settings.setRequirePairingConfirmation(true);
      await _emit(tester, fake, newlyTrusted: true, pairingCode: _code);

      await _tap(tester, find.text('Decline'));
      expect(_decisions(fake), ['reject:tx-new']);
      expect(find.text('The codes match'), findsNothing);
    });

    testWidgets('a device the user has met before is not gated', (
      tester,
    ) async {
      final fake = FakePeerBeam();
      final state = await _pumpTransfers(tester, fake);
      state.settings.setRequirePairingConfirmation(true);
      await _emit(tester, fake, id: 'tx-known');

      await _tap(tester, find.text('Accept'));
      expect(_decisions(fake), ['accept:tx-known']);
      expect(find.text('The codes match'), findsNothing);
    });
  });

  group('with the check OFF (the shipped default)', () {
    testWidgets('a first-contact transfer accepts in one tap, as it always '
        'did', (tester) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);
      await _emit(tester, fake, newlyTrusted: true, pairingCode: _code);

      await _tap(tester, find.text('Accept'));
      // Straight through, and carrying no confirmation — there was nothing to
      // confirm.
      expect(_decisions(fake), ['accept:tx-new']);
    });
  });

  group('the flow itself', () {
    testWidgets('a required confirmation with nothing to show fails closed', (
      tester,
    ) async {
      var accepted = false;
      await tester.pumpWidget(
        MaterialApp(
          home: Builder(
            builder: (context) => TextButton(
              onPressed: () => acceptWithPairingCheck(
                context,
                // No live transfer — a chat offer whose first frame has not
                // landed. There is no code to display, so there is nothing the
                // user could have compared.
                null,
                needsConfirmation: true,
                accept: ({required confirmed}) => accepted = true,
              ),
              child: const Text('go'),
            ),
          ),
        ),
      );
      await _tap(tester, find.text('go'));
      expect(accepted, isFalse);
    });

    testWidgets('no confirmation required is a plain call-through', (
      tester,
    ) async {
      bool? sawConfirmed;
      await tester.pumpWidget(
        MaterialApp(
          home: Builder(
            builder: (context) => TextButton(
              onPressed: () => acceptWithPairingCheck(
                context,
                const Transfer(
                  id: 'tx',
                  peerName: 'Bob',
                  fileName: 'f.bin',
                  direction: TransferDirection.receiving,
                  state: TransferState.pending,
                  totalBytes: 1,
                  doneBytes: 0,
                ),
                needsConfirmation: false,
                accept: ({required confirmed}) => sawConfirmed = confirmed,
              ),
              child: const Text('go'),
            ),
          ),
        ),
      );
      await _tap(tester, find.text('go'));
      // Called, with no confirmation invented on the user's behalf.
      expect(sawConfirmed, isFalse);
      expect(find.text('The codes match'), findsNothing);
    });
  });

  group('the Settings toggle', () {
    testWidgets('is off by default, and round-trips through the engine', (
      tester,
    ) async {
      // Tall enough that the Transfers group is on screen: this tile sits
      // below the fold on the default surface, and a scroll would only make
      // the test about scrolling.
      tester.view.physicalSize = const Size(1200, 3000);
      tester.view.devicePixelRatio = 1.0;
      addTearDown(tester.view.reset);
      final fake = FakePeerBeam();
      final state = AppState.live(fake);
      addTearDown(state.dispose);
      // Attach the store to the engine, as boot does — until it is, a setter
      // updates the in-memory value and persists nothing.
      await state.settings.load(fake);
      await tester.pumpWidget(
        AppScope(
          state: state,
          child: const MaterialApp(home: SettingsScreen()),
        ),
      );
      await tester.pumpAndSettle();

      expect(state.settings.requirePairingConfirmation, isFalse);
      final toggle = find.ancestor(
        of: find.text('Verify new devices with a pairing code'),
        matching: find.byType(SwitchListTile),
      );
      expect(toggle, findsOneWidget);

      await _tap(tester, toggle);
      expect(state.settings.requirePairingConfirmation, isTrue);
      // Persisted under the key the engine and the CLI both read.
      expect(fake.settings['require_pairing_confirmation'], isTrue);

      // And read back: a fresh store loading that document comes up on.
      final reloaded = SettingsStore(
        deviceName: 'd',
        saveDirectory: '/tmp',
        autoAcceptTrusted: false,
        notifications: true,
        compression: true,
      );
      await reloaded.load(fake);
      expect(reloaded.requirePairingConfirmation, isTrue);
    });

    testWidgets('a settings document that predates the check is not consent', (
      tester,
    ) async {
      final fake = FakePeerBeam();
      fake.settings.remove('require_pairing_confirmation');
      final store = SettingsStore(
        deviceName: 'd',
        saveDirectory: '/tmp',
        autoAcceptTrusted: false,
        notifications: true,
        compression: true,
      );
      await store.load(fake);
      expect(store.requirePairingConfirmation, isFalse);
    });
  });
}
