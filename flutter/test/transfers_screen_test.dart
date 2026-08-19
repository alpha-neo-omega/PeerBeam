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
      payload: {'peer': 'Bob', 'file': file ?? '$id.bin', 'incoming': incoming},
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
    AppScope(
      state: state,
      child: const MaterialApp(home: TransfersScreen()),
    ),
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

/// The checkbox on the awaiting-approval card for [id] — there is exactly one
/// per card, found via the same `ValueKey` the list already gives it.
Finder _checkboxFor(String id) => find.descendant(
  of: find.byKey(ValueKey(id)),
  matching: find.byType(Checkbox),
);

/// Tap `Select` and let selection mode's rebuild land.
Future<void> _enterSelectMode(WidgetTester tester) async {
  await tester.tap(find.text('Select'));
  await tester.pump();
}

/// A `transfer_interrupted` for [id] — the event the engine emits when a
/// transfer leaves a checkpoint behind, and the one a restart replays for every
/// checkpoint it finds.
TransferEvent _interrupted(
  String id, {
  required bool resumable,
  String direction = 'sending',
  int done = 700,
  int total = 1000,
}) => TransferEvent(
  kind: 'transfer_interrupted',
  transferId: id,
  timestamp: '',
  payload: {
    'peer_id': 'pb-peer-1',
    'file': '$id.bin',
    'direction': direction,
    'resumable': resumable,
    'stats': {'transferred_bytes': done, 'total_bytes': total},
  },
);

void main() {
  // ── interrupted transfers ──────────────────────────────────────
  //
  // A transfer whose checkpoint outlived it is the one row on this screen that
  // is not running. It must say how far it got, offer the two actions that
  // apply to it, and offer neither of the two that do not.
  group('an interrupted transfer', () {
    testWidgets('shows Resume and Discard, and not Pause or Cancel', (
      tester,
    ) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      fake.emit(_interrupted('t1', resumable: true));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      expect(find.text('Resume'), findsOneWidget);
      expect(find.text('Discard'), findsOneWidget);
      expect(find.text('Interrupted'), findsOneWidget);
      // Nothing to pause and nothing to cancel: the transfer is already over.
      expect(find.byIcon(Icons.pause_rounded), findsNothing);
      expect(find.byIcon(Icons.close_rounded), findsNothing);
      // Nor is it a decision anyone is being asked for.
      expect(find.text('Accept'), findsNothing);
      expect(find.text('Decline'), findsNothing);
    });

    testWidgets('shows how far it got', (tester) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      fake.emit(_interrupted('t1', resumable: true, done: 700, total: 1000));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      // The progress line is `done / total`; a resumable transfer that claimed
      // zero progress would give the user no reason to resume it.
      expect(find.textContaining('700 B / 1000 B'), findsOneWidget);
    });

    testWidgets('Resume calls resumeInterrupted, never resume', (tester) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      fake.emit(_interrupted('t1', resumable: true));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));
      await tester.tap(find.text('Resume'));
      await tester.pump();

      expect(fake.calls, contains('resumeInterrupted:t1'));
      expect(
        fake.calls.where((c) => c == 'resume:t1'),
        isEmpty,
        reason:
            'pb_transfer_resume un-pauses a live transfer — calling it on a '
            'dead one does nothing at all',
      );
    });

    testWidgets('Discard calls discardInterrupted', (tester) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      fake.emit(_interrupted('t1', resumable: true));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));
      await tester.tap(find.text('Discard'));
      await tester.pump();

      expect(fake.calls, contains('discardInterrupted:t1'));
    });

    testWidgets('an inbound one offers Discard and says it is waiting for its '
        'sender, rather than a Resume that would do nothing', (tester) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      fake.emit(_interrupted('t1', resumable: false, direction: 'receiving'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      expect(find.text('Resume'), findsNothing);
      expect(find.text('Waiting for sender'), findsOneWidget);
      expect(find.text('Discard'), findsOneWidget);
    });

    testWidgets('is not counted as work in progress', (tester) async {
      final fake = FakePeerBeam();
      final state = await _pumpTransfers(tester, fake);

      fake.emit(_interrupted('t1', resumable: true));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      expect(
        state.transfer.activeCount,
        0,
        reason:
            'a badge that counted interrupted transfers would sit permanently '
            'lit',
      );
      expect(state.transfer.awaitingApproval, isEmpty);
    });
  });

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

  // Selection mode: instead of only "all of them", the user picks which
  // waiting transfers to answer. `Select` must never pre-select — a
  // pre-filled selection paired with an Accept button would be a batch
  // decision the user did not compose — and the count on screen must always
  // be true, even as engine events keep arriving while the selection is open.
  group('bulk approval selection mode', () {
    testWidgets('"Select" appears alongside the banner, and entering '
        'selection mode starts with nothing checked', (tester) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      fake.emit(_queued('in-1', incoming: true));
      fake.emit(_queued('in-2', incoming: true));
      fake.emit(_queued('in-3', incoming: true));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      expect(find.text('Select'), findsOneWidget);

      await _enterSelectMode(tester);

      expect(find.text('0 of 3 selected'), findsOneWidget);
      expect(find.text('Cancel'), findsOneWidget);
      for (final id in ['in-1', 'in-2', 'in-3']) {
        expect(tester.widget<Checkbox>(_checkboxFor(id)).value, isFalse);
      }
    });

    testWidgets('checkboxes appear only on awaiting-approval cards, and only '
        'while selecting', (tester) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      fake.emit(_queued('in-1', incoming: true));
      fake.emit(_queued('in-2', incoming: true));
      fake.emit(_queued('in-live', incoming: true));
      fake.emit(_started('in-live')); // already approved and running
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      // Not selecting yet: no checkboxes anywhere, on any card.
      expect(find.byType(Checkbox), findsNothing);

      await _enterSelectMode(tester);

      expect(_checkboxFor('in-1'), findsOneWidget);
      expect(_checkboxFor('in-2'), findsOneWidget);
      // Already running: nothing left to decide, so nothing to check.
      expect(_checkboxFor('in-live'), findsNothing);
      expect(find.byType(Checkbox), findsNWidgets(2));
    });

    testWidgets('tapping anywhere on an awaiting-approval card toggles it, '
        'not just the checkbox', (tester) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      fake.emit(_queued('in-1', incoming: true));
      fake.emit(_queued('in-2', incoming: true));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      await _enterSelectMode(tester);
      // The card body, deliberately not its checkbox.
      await tester.tap(find.byKey(const ValueKey('in-1')));
      await tester.pump();

      expect(tester.widget<Checkbox>(_checkboxFor('in-1')).value, isTrue);
      expect(find.text('1 of 2 selected'), findsOneWidget);
    });

    testWidgets('selecting 2 of 3 and tapping Accept calls the engine for '
        'exactly those two ids', (tester) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      fake.emit(_queued('in-1', incoming: true));
      fake.emit(_queued('in-2', incoming: true));
      fake.emit(_queued('in-3', incoming: true));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      await _enterSelectMode(tester);
      await tester.tap(_checkboxFor('in-1'));
      await tester.tap(_checkboxFor('in-3'));
      await tester.pump();
      expect(find.text('2 of 3 selected'), findsOneWidget);

      await tester.tap(find.text('Accept'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));

      // Exactly the two checked ids — in-2 was never touched.
      expect(_decisions(fake), ['accept:in-1', 'accept:in-3']);
    });

    // THE SECURITY TEST, selection-mode's half of it. Same guard as "Accept
    // all": this must call accept and nothing that trusts a device.
    testWidgets('selection-mode accept calls accept — never acceptTrust', (
      tester,
    ) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      fake.emit(_queued('in-1', incoming: true));
      fake.emit(_queued('in-2', incoming: true));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      await _enterSelectMode(tester);
      await tester.tap(_checkboxFor('in-1'));
      await tester.tap(_checkboxFor('in-2'));
      await tester.pump();

      await tester.tap(find.text('Accept'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));

      expect(_decisions(fake), ['accept:in-1', 'accept:in-2']);
      expect(
        fake.calls.where((c) => c.startsWith('acceptTrust:')),
        isEmpty,
        reason: 'selection-mode Accept must never trust a device',
      );
    });

    testWidgets('Decline/Accept are disabled, not just no-ops, with nothing '
        'selected', (tester) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      fake.emit(_queued('in-1', incoming: true));
      fake.emit(_queued('in-2', incoming: true));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      await _enterSelectMode(tester);

      final decline = tester.widget<TextButton>(
        find.widgetWithText(TextButton, 'Decline'),
      );
      final accept = tester.widget<FilledButton>(
        find.widgetWithText(FilledButton, 'Accept'),
      );
      expect(decline.onPressed, isNull);
      expect(accept.onPressed, isNull);

      // Not merely a no-op: nothing reaches the engine either.
      await tester.tap(find.text('Accept'));
      await tester.pump();
      expect(_decisions(fake), isEmpty);
    });

    // The defect requirement 4 exists to prevent: progress events rebuild
    // this screen many times a second, and a selection that reset on the
    // next tick would make selection mode unusable.
    testWidgets('a progress event for an unrelated transfer does not clear '
        'the selection', (tester) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      fake.emit(_queued('in-1', incoming: true));
      fake.emit(_queued('in-2', incoming: true));
      fake.emit(_queued('other', incoming: false)); // our own send
      fake.emit(_started('other'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      await _enterSelectMode(tester);
      await tester.tap(_checkboxFor('in-1'));
      await tester.pump();
      expect(find.text('1 of 2 selected'), findsOneWidget);

      fake.emit(
        TransferEvent(
          kind: 'transfer_progress',
          transferId: 'other',
          timestamp: '',
          payload: const {
            'stats': {'transferred_bytes': 10, 'total_bytes': 100},
          },
        ),
      );
      await tester.pump();

      expect(find.text('1 of 2 selected'), findsOneWidget);
      expect(tester.widget<Checkbox>(_checkboxFor('in-1')).value, isTrue);
    });

    testWidgets('a selected transfer that stops waiting drops out of the '
        'count and is never sent to the engine', (tester) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      fake.emit(_queued('in-1', incoming: true));
      fake.emit(_queued('in-2', incoming: true));
      fake.emit(_queued('in-3', incoming: true));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      await _enterSelectMode(tester);
      await tester.tap(_checkboxFor('in-1'));
      await tester.tap(_checkboxFor('in-2'));
      await tester.pump();
      expect(find.text('2 of 3 selected'), findsOneWidget);

      // in-2 stops waiting from under the selection — started elsewhere, or
      // its own prompt simply timed out. Either way the engine no longer
      // holds a decision open for it.
      fake.emit(_started('in-2'));
      await tester.pump();

      expect(find.text('1 of 2 selected'), findsOneWidget);

      await tester.tap(find.text('Accept'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));

      expect(_decisions(fake), ['accept:in-1']);
      expect(fake.calls.where((c) => c.contains('in-2')), isEmpty);
    });

    testWidgets('"Cancel" exits selection mode and clears the selection', (
      tester,
    ) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      fake.emit(_queued('in-1', incoming: true));
      fake.emit(_queued('in-2', incoming: true));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      await _enterSelectMode(tester);
      await tester.tap(_checkboxFor('in-1'));
      await tester.pump();
      expect(find.text('1 of 2 selected'), findsOneWidget);

      await tester.tap(find.text('Cancel'));
      await tester.pump();

      // Back to the not-selecting banner; per-card actions restored.
      expect(find.text('2 items waiting for approval'), findsOneWidget);
      expect(find.byType(Checkbox), findsNothing);
      expect(find.text('Accept'), findsNWidgets(2));

      // The clear was real, not just hidden: re-entering starts at zero.
      await _enterSelectMode(tester);
      expect(find.text('0 of 2 selected'), findsOneWidget);
    });

    testWidgets('selection-mode partial batch: the snackbar names the real '
        'numbers, and selection mode is left', (tester) async {
      final fake = FakePeerBeam()..noPendingDecisionIds.add('in-2');
      await _pumpTransfers(tester, fake);

      fake.emit(_queued('in-1', incoming: true));
      fake.emit(_queued('in-2', incoming: true));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      await _enterSelectMode(tester);
      await tester.tap(_checkboxFor('in-1'));
      await tester.tap(_checkboxFor('in-2'));
      await tester.pump();

      await tester.tap(find.text('Accept'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));

      expect(
        find.text('Accepted 1 of 2 — 1 was no longer waiting'),
        findsOneWidget,
      );

      // The behaviour that actually distinguishes this path from `Accept all`:
      // a landed selection batch ends selection mode (`if (selecting)
      // widget.onSettled()`). Asserting only the snackbar would pass just as
      // well with the screen left in selection mode, checkboxes still on every
      // card and the per-card Decline/Accept/Trust still gone.
      expect(find.byType(Checkbox), findsNothing, reason: 'selection is over');
      expect(find.text('2 items waiting for approval'), findsOneWidget);
      expect(find.text('Select'), findsOneWidget);
      expect(
        find.text('Trust'),
        findsNWidgets(2),
        reason: 'the per-card actions are back',
      );
    });

    testWidgets('selection mode really ENDS when the banner goes: a later '
        'batch is back in normal mode with nothing checked', (tester) async {
      final fake = FakePeerBeam();
      await _pumpTransfers(tester, fake);

      fake.emit(_queued('in-1', incoming: true));
      fake.emit(_queued('in-2', incoming: true));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      await _enterSelectMode(tester);
      await tester.tap(_checkboxFor('in-1'));
      await tester.tap(_checkboxFor('in-2'));
      await tester.pump();
      expect(find.text('2 of 2 selected'), findsOneWidget);

      // The user walks away. Both prompts are answered elsewhere (or simply
      // time out), the banner drops below its two-waiting threshold and
      // disappears — with `_selecting` still set and `_selected` still full.
      fake.emit(_started('in-1'));
      fake.emit(_started('in-2'));
      await tester.pump(); // the banner goes; the exit is scheduled
      await tester.pump(); // …and lands, after the frame, never during it
      expect(find.byType(Checkbox), findsNothing);

      // Two more arrive — reusing the same ids, which is not contrived:
      // inbound transfer ids come from the SENDER, and `register_vacant` keys
      // them under the peer. A stale selection holding those ids renders both
      // new cards pre-checked, which is exactly the pre-composed batch
      // decision `_enterSelecting`'s reset exists to prevent.
      fake.emit(_queued('in-1', incoming: true));
      fake.emit(_queued('in-2', incoming: true));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 600));

      expect(find.text('2 items waiting for approval'), findsOneWidget);
      expect(
        find.byType(Checkbox),
        findsNothing,
        reason: 'the banner must come back in NORMAL mode',
      );
      expect(find.text('Select'), findsOneWidget);
      expect(
        find.text('Trust'),
        findsNWidgets(2),
        reason: 'and the per-card Decline/Accept/Trust must be back',
      );

      // The clear was real, not merely masked: entering again starts at zero.
      await _enterSelectMode(tester);
      expect(find.text('0 of 2 selected'), findsOneWidget);
    });
  });
}
