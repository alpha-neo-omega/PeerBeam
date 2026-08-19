import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/timeline/timeline_screen.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

Future<void> _open(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: const MaterialApp(home: TimelineScreen()),
    ),
  );
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 300));
}

TimelineEvent _e(
  String kind, {
  String peer = '',
  String detail = '',
  bool ok = true,
}) => TimelineEvent(
  kind: kind,
  at: DateTime.now(),
  peer: peer,
  detail: detail,
  ok: ok,
);

void main() {
  testWidgets('an empty timeline says so rather than showing a blank page', (
    tester,
  ) async {
    await _open(tester, FakePeerBeam());
    expect(find.text('Nothing yet'), findsOneWidget);
  });

  testWidgets('each kind of activity names the device involved', (
    tester,
  ) async {
    // "A message" tells the reader nothing they did not already know.
    final fake = FakePeerBeam()
      ..timelineEvents.addAll([
        _e('transfer', peer: 'pb-bob', detail: 'report.pdf'),
        _e('chat', peer: 'pb-bob'),
        _e('clipboard', peer: 'pb-bob'),
        _e('clipboard'),
      ]);
    await _open(tester, fake);

    expect(find.text('report.pdf — pb-bob'), findsOneWidget);
    expect(find.text('Message with pb-bob'), findsOneWidget);
    expect(find.text('Clipboard from pb-bob'), findsOneWidget);
    expect(find.text('Clipboard copied here'), findsOneWidget);
  });

  testWidgets('a failed transfer is marked as failed', (tester) async {
    final fake = FakePeerBeam()
      ..timelineEvents.add(
        _e('transfer', peer: 'pb-bob', detail: 'big.iso', ok: false),
      );
    await _open(tester, fake);
    expect(find.text('big.iso — pb-bob (failed)'), findsOneWidget);
  });
}
