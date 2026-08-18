import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/clipboard/clipboard_history_screen.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

Future<AppState> _open(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: const MaterialApp(home: ClipboardHistoryScreen()),
    ),
  );
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 300));
  return state;
}

ClipEntry _entry(String text, {String? from}) => ClipEntry(
  id: '1',
  text: text,
  from: from,
  at: DateTime.now(),
);

void main() {
  testWidgets('an empty screen says whether history is off or merely empty', (
    tester,
  ) async {
    // Two different facts. A user staring at an empty screen deserves to know
    // which one they are looking at.
    final fake = FakePeerBeam();
    final state = await _open(tester, fake);
    expect(find.text('Clipboard history is off'), findsOneWidget);

    state.settings.setClipboardHistory(true);
    await tester.pump();
    expect(find.text('Nothing remembered yet'), findsOneWidget);
  });

  testWidgets('a long clip is previewed, never rendered whole', (tester) async {
    // Fifty entries rendered in full would put every remembered password on
    // one page, which is what bounding the log was meant to avoid.
    final secret = 's3cr3t-${'x' * 500}';
    final fake = FakePeerBeam()..clipHistory.add(_entry(secret));
    await _open(tester, fake);

    expect(find.text(secret), findsNothing);
    expect(find.textContaining('s3cr3t-'), findsOneWidget);
  });

  testWidgets('an entry says whether it came from here or from a device', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..clipHistory.addAll([
        _entry('mine'),
        _entry('theirs', from: 'pb-bob'),
      ]);
    await _open(tester, fake);

    expect(find.text('Copied here'), findsOneWidget);
    expect(find.text('From pb-bob'), findsOneWidget);
  });

  testWidgets('erasing asks first, and only then erases', (tester) async {
    final fake = FakePeerBeam()..clipHistory.add(_entry('remembered'));
    await _open(tester, fake);

    await tester.tap(find.byIcon(Icons.delete_sweep_outlined));
    await tester.pumpAndSettle();
    expect(find.text('Erase clipboard history?'), findsOneWidget);

    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(
      fake.calls.contains('clipboardHistoryClear'),
      isFalse,
      reason: 'cancelling still erased the history',
    );

    await tester.tap(find.byIcon(Icons.delete_sweep_outlined));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Erase'));
    await tester.pumpAndSettle();
    expect(fake.calls, contains('clipboardHistoryClear'));
    expect(find.textContaining('remembered'), findsNothing);
  });
}
