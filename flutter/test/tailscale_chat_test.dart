// Opening a conversation with a peer known only by a provider's own name.
//
// A Tailscale device reaches the app as `ts:<node id>`, which is Tailscale's
// name for it and not PeerBeam's — and the engine's chat store refuses it
// outright, because a colon is not a legal namespace character. The real id
// cannot be guessed, so tapping chat dials the address and asks
// (`peerIdentify`). That dial is the subject of this file.
//
// Three things about it are load-bearing and each was wrong at some point:
//
//  1. It completes an authenticated handshake, which on **first contact pins
//     the peer's key**. The CLI prints the pairing code and says to compare it.
//     The GUI pinned in silence, so the one check that detects an interception
//     could not be performed.
//  2. It blocks for as long as a dial takes — up to 8s per resolved address,
//     and 120s if a peer answers and then stalls. It ran on the UI isolate, so
//     that was not a spinner but a frozen app.
//  3. A failure said "could not reach it" whatever the cause — a refusal, a
//     withheld permission and an engine that was not running all read as a
//     network problem the user then went looking for.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/data/discovery_repository.dart';
import 'package:peerbeam/features/chat/chat_screen.dart';
import 'package:peerbeam/features/home/home_screen.dart';
import 'package:peerbeam/sdk/error_text.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/exceptions.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';
import 'package:peerbeam/widgets/pairing.dart';

import 'package:shared_preferences/shared_preferences.dart';

import 'sdk/fake_peerbeam.dart';

/// Exactly the id shape `peerbeam-discovery-tailscale` mints
/// (status.rs: `DeviceId::from(format!("ts:{raw_id}"))`).
const _tailscale = SdkDevice(
  id: 'ts:nodeidabc123',
  name: 'alice-laptop',
  kind: 'laptop',
  platform: 'linux',
  addresses: ['100.101.102.103'],
  port: 49600,
  online: true,
  latencyMs: 12,
  reachableLan: false,
  reachableRemote: true,
);

const _identityKey = '100.101.102.103:49600';

Future<AppState> _pump(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: const MaterialApp(home: HomeScreen()),
    ),
  );
  await tester.pump();
  fake.emit(const DeviceAdded(_tailscale));
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 300));
  expect(find.text('alice-laptop'), findsOneWidget);
  return state;
}

/// Tap chat and let the dial finish.
///
/// Not `pumpAndSettle`: `withProcessing` puts a `CircularProgressIndicator` up
/// while the dial runs, and that animation never settles — so a settle would
/// time out on the very state this is waiting through. Pumped by hand instead,
/// enough frames for the spinner to be raised, the future to complete, and the
/// dialog that replaces it to be built.
Future<void> _tapChat(WidgetTester tester) async {
  await tester.tap(find.byTooltip('Chat with alice-laptop'));
  for (var i = 0; i < 6; i++) {
    await tester.pump(const Duration(milliseconds: 200));
  }
}

void main() {
  // Every path here records what a dial learned, and that write goes to
  // shared_preferences — so the whole file needs a store, not just the group
  // that reads one back.
  setUp(() => SharedPreferences.setMockInitialValues({}));

  group('first contact is shown, not swallowed', () {
    testWidgets('the pairing code and what to do with it are put on screen', (
      tester,
    ) async {
      final fake = FakePeerBeam();
      fake.identities[_identityKey] = const PeerIdentity(
        deviceId: 'pb-alice-laptop',
        name: 'alice-laptop',
        newlyTrusted: true,
        pairingCode: '4829 1374 5561 0928',
      );
      await _pump(tester, fake);
      await _tapChat(tester);

      expect(find.text(firstContactTitle), findsOneWidget);
      // The whole code, never abbreviated — all 128 bits are what make it
      // expensive to forge.
      expect(find.text('4829 1374 5561 0928'), findsOneWidget);
      expect(find.text(pairingCodeInstruction), findsOneWidget);
      // And it must not be entered without a decision.
      expect(find.byType(ChatScreen), findsNothing);
    });

    testWidgets('confirming opens the conversation under the answered id', (
      tester,
    ) async {
      final fake = FakePeerBeam();
      fake.identities[_identityKey] = const PeerIdentity(
        deviceId: 'pb-alice-laptop',
        name: 'alice-laptop',
        newlyTrusted: true,
        pairingCode: '4829 1374 5561 0928',
      );
      await _pump(tester, fake);
      await _tapChat(tester);

      await tester.tap(find.text('Open conversation'));
      for (var i = 0; i < 6; i++) {
        await tester.pump(const Duration(milliseconds: 200));
      }

      expect(find.byType(ChatScreen), findsOneWidget);
      final screen = tester.widget<ChatScreen>(find.byType(ChatScreen));
      // The ANSWERED id, never the `ts:` one we started from — that one is
      // what the store refuses.
      expect(screen.peerId, 'pb-alice-laptop');
    });

    // The pin has already happened by the time the code is shown — the
    // handshake did it. So the action that matters is the one that undoes it.
    testWidgets('forgetting drops the key and does not open the thread', (
      tester,
    ) async {
      final fake = FakePeerBeam();
      fake.identities[_identityKey] = const PeerIdentity(
        deviceId: 'pb-alice-laptop',
        name: 'alice-laptop',
        newlyTrusted: true,
        pairingCode: '4829 1374 5561 0928',
      );
      fake.trusted = [
        TrustedDevice(
          id: 'pb-alice-laptop',
          name: 'alice-laptop',
          fingerprint: 'ab:cd',
          trustedAt: DateTime.utc(2026, 1, 1),
          approved: false,
        ),
      ];
      await _pump(tester, fake);
      await _tapChat(tester);

      await tester.tap(find.text('Forget it'));
      for (var i = 0; i < 6; i++) {
        await tester.pump(const Duration(milliseconds: 200));
      }

      expect(find.byType(ChatScreen), findsNothing);
      expect(
        fake.trusted.where((t) => t.id == 'pb-alice-laptop'),
        isEmpty,
        reason: 'the pin this handshake wrote is gone again',
      );
    });

    // Dismissing without the check being *required* is backing out, not
    // discarding a key. Only a required confirmation treats silence as a no.
    testWidgets('a device met before opens straight through, no dialog', (
      tester,
    ) async {
      final fake = FakePeerBeam();
      fake.identities[_identityKey] = const PeerIdentity(
        deviceId: 'pb-alice-laptop',
        name: 'alice-laptop',
        newlyTrusted: false,
        pairingCode: '',
      );
      await _pump(tester, fake);
      await _tapChat(tester);

      expect(find.text(firstContactTitle), findsNothing);
      expect(find.byType(ChatScreen), findsOneWidget);
    });
  });

  // The thread is filed under the id that ANSWERED, and discovery only ever
  // reports `ts:<node>`. Without a mapping between the two, reopening the
  // conversation asked for a target under an id discovery has never heard of,
  // got null, and rendered a disabled composer that said the peer was
  // unreachable — while that peer sat online in the device list one screen
  // away. The conversation was openable exactly once.
  group('a resolved identity is remembered', () {
    testWidgets('the thread can find its peer again after being left', (
      tester,
    ) async {
      final fake = FakePeerBeam();
      fake.identities[_identityKey] = const PeerIdentity(
        deviceId: 'pb-alice-laptop',
        name: 'alice-laptop',
        newlyTrusted: false,
        pairingCode: '',
      );
      final state = await _pump(tester, fake);
      await _tapChat(tester);
      expect(find.byType(ChatScreen), findsOneWidget);

      // Reopening from Conversations asks by the ANSWERED id, which is all a
      // thread knows. That must still resolve to a live, sendable route.
      final target = state.device.peerTarget('pb-alice-laptop');
      expect(target, isNotNull);
      expect(target!.addresses, ['100.101.102.103']);
      expect(target.port, 49600);
      // Carrying the answered id, never `ts:` — the engine files the
      // conversation by this, and its store refuses a colon.
      expect(target.id, 'pb-alice-laptop');
    });

    testWidgets('a second visit opens without dialling again', (tester) async {
      final fake = FakePeerBeam();
      fake.identities[_identityKey] = const PeerIdentity(
        deviceId: 'pb-alice-laptop',
        name: 'alice-laptop',
        newlyTrusted: false,
        pairingCode: '',
      );
      await _pump(tester, fake);
      await _tapChat(tester);
      expect(find.byType(ChatScreen), findsOneWidget);

      await tester.pageBack();
      // Pumped by hand: the chat screen keeps animations alive, so a settle
      // waits on something that never finishes.
      for (var i = 0; i < 8; i++) {
        await tester.pump(const Duration(milliseconds: 200));
      }
      await _tapChat(tester);

      expect(find.byType(ChatScreen), findsOneWidget);
      expect(
        fake.calls.where((c) => c.startsWith('peerIdentify:')).length,
        1,
        reason: 'the answer was already known; asking again costs a connection',
      );
    });

    testWidgets('what was learned survives a restart', (tester) async {
      final fake = FakePeerBeam();
      fake.identities[_identityKey] = const PeerIdentity(
        deviceId: 'pb-alice-laptop',
        name: 'alice-laptop',
        newlyTrusted: false,
        pairingCode: '',
      );
      await _pump(tester, fake);
      await _tapChat(tester);
      expect(find.byType(ChatScreen), findsOneWidget);

      // A fresh repository over the same store — what the next launch builds.
      final next = DiscoveryRepository(api: FakePeerBeam());
      addTearDown(next.dispose);
      await next.loadIdentities();

      expect(next.resolvedIdFor('ts:nodeidabc123'), 'pb-alice-laptop');
    });

    // A conversation is local history. Needing the peer up to read what it
    // already said would be backwards, and a dial cannot succeed anyway.
    testWidgets('an offline peer still opens its thread, read-only', (
      tester,
    ) async {
      SharedPreferences.setMockInitialValues({
        'resolved_identities_v1': '{"ts:nodeidabc123":"pb-alice-laptop"}',
      });
      final fake = FakePeerBeam();
      final state = AppState.live(fake);
      addTearDown(state.dispose);
      await state.device.loadIdentities();
      await tester.pumpWidget(
        AppScope(
          state: state,
          child: const MaterialApp(home: HomeScreen()),
        ),
      );
      await tester.pump();
      fake.emit(const DeviceAdded(_tailscale));
      fake.emit(const DeviceStatusChanged('ts:nodeidabc123', false));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));

      await _tapChat(tester);

      expect(find.byType(ChatScreen), findsOneWidget);
      expect(
        fake.calls.where((c) => c.startsWith('peerIdentify:')),
        isEmpty,
        reason: 'nothing was dialled — the answer was already on disk',
      );
    });
  });

  group('the dial itself', () {
    // A responsive app is one whose button can be pressed again. Before the
    // call went off the UI isolate the second tap was impossible because
    // nothing was painting; now it is possible, so it has to be refused.
    testWidgets('tapping chat twice dials once', (tester) async {
      final fake = FakePeerBeam();
      fake.identities[_identityKey] = const PeerIdentity(
        deviceId: 'pb-alice-laptop',
        name: 'alice-laptop',
        newlyTrusted: false,
        pairingCode: '',
      );
      await _pump(tester, fake);

      await tester.tap(find.byTooltip('Chat with alice-laptop'));
      await tester.tap(find.byTooltip('Chat with alice-laptop'));
      for (var i = 0; i < 8; i++) {
        await tester.pump(const Duration(milliseconds: 200));
      }

      expect(
        fake.calls.where((c) => c.startsWith('peerIdentify:')).length,
        1,
        reason: 'a second press must not open a second connection',
      );
      expect(find.byType(ChatScreen), findsOneWidget);
    });

    // The raw engine string is deliberately never shown (`error_text.dart`
    // exists to stop that). What changed is that the *class* of failure now
    // reaches the surface at all: this used to be one hardcoded sentence about
    // reachability whatever had actually gone wrong.
    testWidgets('an unreachable address is reported as unreachable', (
      tester,
    ) async {
      final fake = FakePeerBeam();
      // No entry for this address, so the fake refuses the way the engine
      // does — with a ConnectionException.
      await _pump(tester, fake);
      await _tapChat(tester);

      expect(find.byType(ChatScreen), findsNothing);
      expect(
        find.textContaining(
          friendlyError(const ConnectionException('unreachable')),
        ),
        findsOneWidget,
      );
      // Named, so a person with several devices knows which one answered.
      expect(find.textContaining('alice-laptop'), findsWidgets);
    });

    // Not a network problem, and no longer reported as one.
    testWidgets('a failure that is not the network is not blamed on it', (
      tester,
    ) async {
      final fake = FakePeerBeam();
      fake.failing.add('peerIdentify'); // throws a StateError, not a refusal
      await _pump(tester, fake);
      await _tapChat(tester);

      expect(find.byType(ChatScreen), findsNothing);
      expect(
        find.textContaining(
          friendlyError(const ConnectionException('unreachable')),
        ),
        findsNothing,
        reason: 'nothing here says the two devices are on different networks',
      );
      expect(find.textContaining('Something went wrong'), findsOneWidget);
    });
  });
}
