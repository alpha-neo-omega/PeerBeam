// A read that fails must never render as an absence.
//
// Five screens loaded their data in an unguarded `_load()`. The throw escaped
// the post-frame callback that started it, the loading flag never flipped back,
// and the body stayed an empty box — permanently, with no message and no way to
// retry. Browsing a device that has gone to sleep is the *ordinary* case for a
// peer-to-peer app on a LAN, and it produced something indistinguishable from a
// crash.
//
// Each test here fails the underlying read and asserts the screen says so and
// offers a way back. The pairing matters: "Nothing shared" and "could not read
// what is shared" are different facts, and showing the first when the second is
// true tells someone they share nothing while they may share plenty.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/features/clipboard/clipboard_history_screen.dart';
import 'package:peerbeam/features/settings/logs_screen.dart';
import 'package:peerbeam/features/timeline/timeline_screen.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

Future<AppState> _open(
  WidgetTester tester,
  FakePeerBeam fake,
  Widget screen,
) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: MaterialApp(home: screen),
    ),
  );
  await tester.pumpAndSettle();
  return state;
}

void main() {
  testWidgets('a failed log read says so and offers a retry', (tester) async {
    final fake = FakePeerBeam()..failing.add('logs');
    await _open(tester, fake, const LogsScreen());

    expect(find.text('Could not read the logs'), findsOneWidget);
    expect(find.text('Try again'), findsOneWidget);
    expect(
      find.text('Nothing logged yet'),
      findsNothing,
      reason: 'a failure must not be reported as an empty buffer',
    );
  });

  testWidgets('retrying after a failure recovers', (tester) async {
    final fake = FakePeerBeam()..failing.add('logs');
    await _open(tester, fake, const LogsScreen());
    expect(find.text('Try again'), findsOneWidget);

    // The engine comes back — the usual reason a read failed.
    fake.failing.remove('logs');
    await tester.tap(find.text('Try again'));
    await tester.pumpAndSettle();

    expect(find.text('Could not read the logs'), findsNothing);
  });

  testWidgets('a failed clipboard read says so', (tester) async {
    final fake = FakePeerBeam()..failing.add('clipboardHistory');
    await _open(tester, fake, const ClipboardHistoryScreen());

    expect(find.text('Could not read clipboard history'), findsOneWidget);
    expect(find.text('Try again'), findsOneWidget);
  });

  testWidgets('a failed activity read says so', (tester) async {
    final fake = FakePeerBeam()..failing.add('timeline');
    await _open(tester, fake, const TimelineScreen());

    expect(find.text('Could not read activity'), findsOneWidget);
    expect(find.text('Try again'), findsOneWidget);
  });

  testWidgets('a screen whose read succeeds shows no error', (tester) async {
    await _open(tester, FakePeerBeam(), const TimelineScreen());
    expect(find.text('Could not read activity'), findsNothing);
    expect(find.text('Try again'), findsNothing);
  });
}
