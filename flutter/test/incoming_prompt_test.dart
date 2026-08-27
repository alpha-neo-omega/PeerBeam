// The incoming-transfer prompt: the approval question raised over whatever is
// on screen.
//
// Four things are load-bearing and each has its own test:
//   1. It appears at all, from a screen that is not Transfers — the whole point,
//      since approval used to be reachable only by knowing which tab to open.
//   2. Dismissing answers NOTHING. Not accept, not decline. A prompt that
//      decided something on an accidental tap outside would be worse than no
//      prompt, and one that accepted would be a security hole.
//   3. The setting genuinely suppresses it, and suppressing it still leaves the
//      transfer waiting rather than accepting it.
//   4. One at a time, and never twice for the same transfer.

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/transfers/incoming_prompt.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

/// A `transfer_queued` for [id]; inbound ones land in `pending`, which is the
/// only state that awaits an answer.
TransferEvent _queued(String id, {bool incoming = true, String? file}) =>
    TransferEvent(
      kind: 'transfer_queued',
      transferId: id,
      timestamp: '',
      payload: {'peer': 'Bob', 'file': file ?? '$id.bin', 'incoming': incoming},
    );

/// Mount the prompt over a stand-in screen that is deliberately **not**
/// Transfers — the case the prompt exists for.
Future<AppState> _pump(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: const MaterialApp(
        home: IncomingTransferPrompt(
          child: Scaffold(body: Center(child: Text('some other screen'))),
        ),
      ),
    ),
  );
  await tester.pump();
  return state;
}

void main() {
  testWidgets('an arriving file raises a prompt over an unrelated screen', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    final state = await _pump(tester, fake);

    expect(find.text('Incoming file'), findsNothing);

    fake.emit(_queued('in-1', file: 'holiday.zip'));
    await tester.pumpAndSettle();

    expect(find.text('Incoming file'), findsOneWidget);
    expect(find.text('holiday.zip'), findsOneWidget);
    expect(find.textContaining('Bob'), findsOneWidget);
    // Still unanswered: showing the question decides nothing.
    expect(state.transfer.awaitingApproval, hasLength(1));
  });

  testWidgets('dismissing the prompt answers nothing at all', (tester) async {
    final fake = FakePeerBeam();
    final state = await _pump(tester, fake);
    fake.emit(_queued('in-1'));
    await tester.pumpAndSettle();
    expect(find.text('Incoming file'), findsOneWidget);

    // Tap the barrier, the way a stray tap outside the dialog would.
    await tester.tapAt(const Offset(10, 10));
    await tester.pumpAndSettle();

    expect(find.text('Incoming file'), findsNothing);
    // The engine was never told anything...
    expect(
      fake.calls.where(
        (c) =>
            c.startsWith('accept:') ||
            c.startsWith('acceptTrust:') ||
            c.startsWith('reject:'),
      ),
      isEmpty,
      reason: 'a dismissed prompt must not decide',
    );
    // ...and the transfer is still waiting, on the Transfers screen.
    expect(state.transfer.awaitingApproval, hasLength(1));
  });

  testWidgets('Decline declines, and only the one it was raised for', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    await _pump(tester, fake);
    fake.emit(_queued('in-1'));
    await tester.pumpAndSettle();

    await tester.tap(find.text('Decline'));
    await tester.pumpAndSettle();

    expect(fake.calls.where((c) => c.startsWith('reject:')), ['reject:in-1']);
    expect(fake.calls.where((c) => c.startsWith('accept')), isEmpty);
  });

  testWidgets('Accept accepts once and never trusts', (tester) async {
    final fake = FakePeerBeam();
    await _pump(tester, fake);
    fake.emit(_queued('in-1'));
    await tester.pumpAndSettle();

    await tester.tap(find.text('Accept'));
    await tester.pumpAndSettle();

    // `accept`, never `acceptTrust`: Trust is a lasting grant and must stay a
    // button of its own, exactly as it is on the Transfers card.
    expect(fake.calls.where((c) => c.startsWith('accept:')), hasLength(1));
    expect(fake.calls.where((c) => c.startsWith('acceptTrust:')), isEmpty);
  });

  testWidgets('with the setting off, nothing is raised and nothing is decided', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    final state = await _pump(tester, fake);
    // Not awaited on purpose. The flag flips synchronously and only the
    // persistence behind it is async — and `SharedPreferences` has no platform
    // implementation in a widget test, so awaiting the write hangs the test
    // for the full ten-minute timeout instead of failing it.
    unawaited(state.view.setAskOnReceive(false));
    await tester.pump();

    fake.emit(_queued('in-1'));
    await tester.pumpAndSettle();

    expect(find.text('Incoming file'), findsNothing);
    // The distinction the subtitle promises: off means "do not interrupt me",
    // never "accept it for me".
    expect(
      fake.calls.where(
        (c) => c.startsWith('accept') || c.startsWith('reject:'),
      ),
      isEmpty,
    );
    expect(state.transfer.awaitingApproval, hasLength(1));
  });

  testWidgets('a dismissed transfer is not raised again', (tester) async {
    final fake = FakePeerBeam();
    final state = await _pump(tester, fake);
    fake.emit(_queued('in-1'));
    await tester.pumpAndSettle();
    await tester.tapAt(const Offset(10, 10));
    await tester.pumpAndSettle();
    expect(find.text('Incoming file'), findsNothing);

    // Any further notification from the store — progress elsewhere, another
    // transfer changing state — must not resurrect it. A prompt that came back
    // every time anything moved could not be got rid of without answering it,
    // which is the coercion a dismissable prompt must not become.
    fake.emit(_queued('out-1', incoming: false));
    await tester.pumpAndSettle();

    expect(find.text('Incoming file'), findsNothing);
    expect(state.transfer.awaitingApproval, hasLength(1));
  });

  testWidgets('two arrivals are asked one at a time', (tester) async {
    final fake = FakePeerBeam();
    await _pump(tester, fake);

    fake.emit(_queued('in-1', file: 'first.bin'));
    fake.emit(_queued('in-2', file: 'second.bin'));
    await tester.pumpAndSettle();

    // One dialog, not two stacked past the point of being answerable.
    expect(find.text('Incoming file'), findsOneWidget);
    expect(find.text('first.bin'), findsOneWidget);
    expect(find.text('second.bin'), findsNothing);
  });
}
