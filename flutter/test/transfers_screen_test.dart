// The bulk-approval banner on the Transfers screen: when it is allowed to
// exist, what it is allowed to do, and what it is allowed to claim afterwards.
//
// Three things are load-bearing here and each has its own test:
//   1. It stays hidden until a batch is actually a batch (two or more), because
//      over a single card it is just a second button saying the same thing.
//   2. "Accept all" ACCEPTS. It never trusts, and it never reaches past the
//      inbound transfers that are genuinely waiting. Asserted against the
//      fake's call log, so a regression to `acceptTrust` — a lasting grant of
//      auto-accept for every future send from that device — fails here.
//   3. What it reports afterwards is counted from decisions the engine
//      answered, never from the number that was on screen when it was tapped.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/transfers/transfers_screen.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

/// A `transfer_queued` for [id]; inbound ones land in `pending` — awaiting the
/// user's approval — which is the only state the banner cares about.
TransferEvent _queued(String id, {required bool incoming, String? file}) =>
    TransferEvent(
      kind: 'transfer_queued',
      transferId: id,
      timestamp: '',
      payload: {
        'peer': 'Bob',
        'file': file ?? '$id.bin',
        'incoming': incoming,
      },
    );

TransferEvent _started(String id) => TransferEvent(
  kind: 'transfer_started',
  transferId: id,
  timestamp: '',
  payload: const {},
);

/// Pump the Transfers screen against [fake] and settle the entrance animation.
Future<AppState> _pumpTransfers(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(state: state, child: const MaterialApp(home: TransfersScreen())),
  );
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 600));
  return state;
}

/// Tap [label] on the banner and let the batch decision land.
Future<void> _tapBanner(WidgetTester tester, String label) async {
  await tester.tap(find.text(label));
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 300));
}

/// Only the approval decisions the engine was asked to make.
List<String> _decisions(FakePeerBeam fake) => fake.calls
    .where(
      (c) =>
          c.startsWith('accept:') ||
          c.startsWith('acceptTrust:') ||
          c.startsWith('reject:'),
    )
    .toList();

void main() {
  group('bulk approval banner visibility', () {
    testWidgets('hidden when nothing is waiting', (tester) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      // An inbound transfer the user already approved: it is running, so its
      // decision is made and there is nothing to batch.
      fake.emit(_queued('in-live', incoming: true));
      fake.emit(_started('in-live'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      expect(find.text('Accept all'), findsNothing);
      expect(find.text('Decline all'), findsNothing);
      expect(find.textContaining('waiting for approval'), findsNothing);
    });

    testWidgets('hidden with exactly one waiting transfer — its own card is '
        'already the shortest path', (tester) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      fake.emit(_queued('in-1', incoming: true));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      expect(find.text('Accept all'), findsNothing);
      expect(find.text('Decline all'), findsNothing);
      // The per-card actions are untouched and still the way to answer it.
      expect(find.text('Accept'), findsOneWidget);
      expect(find.text('Decline'), findsOneWidget);
      expect(find.text('Trust'), findsOneWidget);
    });

    testWidgets('shown from two waiting transfers up, alongside — never '
        'instead of — the per-card actions', (tester) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      fake.emit(_queued('in-1', incoming: true));
      fake.emit(_queued('in-2', incoming: true));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      expect(find.text('2 items waiting for approval'), findsOneWidget);
      expect(find.text('Accept all'), findsOneWidget);
      expect(find.text('Decline all'), findsOneWidget);
      // Each card keeps its own three actions.
      expect(find.text('Accept'), findsNWidgets(2));
      expect(find.text('Decline'), findsNWidgets(2));
      expect(find.text('Trust'), findsNWidgets(2));
      // And there is exactly one place to grant lasting trust: the card.
      expect(find.text('Trust all'), findsNothing);
    });

    testWidgets('two outbound sends are not a batch — nothing there was ever '
        "the user's to approve", (tester) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      fake.emit(_queued('out-1', incoming: false));
      fake.emit(_queued('out-2', incoming: false));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      expect(find.text('Accept all'), findsNothing);
    });
  });

  group('bulk approval actions', () {
    // THE SECURITY TEST. `acceptTrust` grants a device persistent auto-accept
    // for everything it sends from then on; "Accept all" answers only the
    // batch on screen. If this ever calls acceptTrust, or reaches a transfer
    // that was not an inbound one awaiting approval, this fails.
    testWidgets('"Accept all" calls accept — never acceptTrust — once per '
        'waiting inbound transfer, and touches nothing else', (tester) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      fake.emit(_queued('in-1', incoming: true));
      fake.emit(_queued('out-1', incoming: false)); // our own send
      fake.emit(_queued('in-2', incoming: true));
      fake.emit(_queued('in-live', incoming: true));
      fake.emit(_started('in-live')); // already approved and running
      fake.emit(_queued('in-3', incoming: true));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      await _tapBanner(tester, 'Accept all');

      // Exactly the three waiting inbound transfers, one plain accept each.
      expect(_decisions(fake), ['accept:in-1', 'accept:in-2', 'accept:in-3']);
      // Said again, on its own, because this is the clause that matters: no
      // trust was granted to anything.
      expect(
        fake.calls.where((c) => c.startsWith('acceptTrust:')),
        isEmpty,
        reason: '"Accept all" must never trust a device',
      );
      // The outbound send and the running transfer were never addressed.
      expect(fake.calls.where((c) => c.contains('out-1')), isEmpty);
      expect(fake.calls.where((c) => c.contains('in-live')), isEmpty);
    });

    testWidgets('"Decline all" is symmetric: reject once per waiting inbound '
        'transfer, and nothing else', (tester) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      fake.emit(_queued('in-1', incoming: true));
      fake.emit(_queued('out-1', incoming: false));
      fake.emit(_queued('in-2', incoming: true));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      await _tapBanner(tester, 'Decline all');

      expect(_decisions(fake), ['reject:in-1', 'reject:in-2']);
      expect(fake.calls.where((c) => c.contains('out-1')), isEmpty);
      expect(find.text('Declined 2 items'), findsOneWidget);
    });
  });

  group('bulk approval reporting', () {
    testWidgets('all of them went through: the count is the batch', (
      tester,
    ) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      for (final id in ['in-1', 'in-2', 'in-3']) {
        fake.emit(_queued(id, incoming: true));
      }
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      await _tapBanner(tester, 'Accept all');

      expect(find.byType(SnackBar), findsOneWidget);
      expect(find.text('Accepted 3 items'), findsOneWidget);
    });

    // The race this feature has to survive. Between the banner rendering and
    // the tap a transfer can time out, its sender can give up, or the user can
    // answer it from its own card — and the engine then refuses the decision.
    // Reporting "Accepted 5" would be a claim; five separate error snackbars
    // would read as breakage. One line, counted from what came back.
    testWidgets('some had stopped waiting: the report names the real numbers', (
      tester,
    ) async {
      final fake = FakePeerBeam()
        ..noPendingDecisionIds.addAll(['in-2', 'in-5']);
      await _pumpTransfers(tester, fake);

      for (final id in ['in-1', 'in-2', 'in-3', 'in-4', 'in-5']) {
        fake.emit(_queued(id, incoming: true));
      }
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      await _tapBanner(tester, 'Accept all');

      // All five were asked; only three could be answered.
      expect(_decisions(fake), hasLength(5));
      expect(find.byType(SnackBar), findsOneWidget);
      expect(
        find.text('Accepted 3 of 5 — 2 were no longer waiting'),
        findsOneWidget,
      );
      // One line, not one per failure.
      expect(find.textContaining("Couldn't accept"), findsNothing);
    });

    testWidgets('one had stopped waiting: singular, not "1 were"', (
      tester,
    ) async {
      final fake = FakePeerBeam()..noPendingDecisionIds.add('in-2');
      await _pumpTransfers(tester, fake);

      fake.emit(_queued('in-1', incoming: true));
      fake.emit(_queued('in-2', incoming: true));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      await _tapBanner(tester, 'Decline all');

      expect(
        find.text('Declined 1 of 2 — 1 was no longer waiting'),
        findsOneWidget,
      );
    });

    testWidgets('the whole batch had gone: says so, and claims no successes', (
      tester,
    ) async {
      final fake = FakePeerBeam()
        ..noPendingDecisionIds.addAll(['in-1', 'in-2', 'in-3']);
      await _pumpTransfers(tester, fake);

      for (final id in ['in-1', 'in-2', 'in-3']) {
        fake.emit(_queued(id, incoming: true));
      }
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      await _tapBanner(tester, 'Accept all');

      expect(find.text('None were still waiting'), findsOneWidget);
      expect(find.textContaining('Accepted'), findsNothing);
    });
  });
}
