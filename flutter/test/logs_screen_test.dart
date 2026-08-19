// The Logs screen: the engine's buffer, made reachable.
//
// What the screen has to get right, and what these tests pin:
//
//  1. **An empty buffer says so.** A blank page is what a broken screen looks
//     like, and "nothing has happened yet" is a different piece of news.
//  2. **Problems are distinguishable without reading.** Nobody opens a log to
//     read it; they open it to find the two lines that went wrong. The engine
//     already answers which those are (`LogLine.isProblem`), so the level
//     strings are never parsed in the UI.
//  3. **Export names the file.** The whole point is attaching logs to a bug
//     report, and "Exported" with no path is a file nobody can find.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/settings/logs_screen.dart';
import 'package:peerbeam/features/settings/settings_screen.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

LogLine _line(String level, String message) => LogLine(
  at: '2026-08-19T10:04:12.123Z',
  level: level,
  target: 'pb',
  message: message,
);

Future<void> _open(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: const MaterialApp(home: LogsScreen()),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  /// **An empty buffer is a fact, and gets stated.** Rendering nothing would
  /// leave the reader unable to tell "no problems yet" from "this screen is
  /// broken".
  testWidgets('an empty buffer says so instead of showing a blank page', (
    tester,
  ) async {
    await _open(tester, FakePeerBeam());

    expect(find.text('Nothing logged yet'), findsOneWidget);
    expect(
      find.textContaining('start empty each time it starts'),
      findsOneWidget,
    );
  });

  /// Oldest first, newest last: a log line only means anything beside the ones
  /// around it, so the engine's order is kept rather than reversed.
  testWidgets('lines render in engine order, newest last', (tester) async {
    final fake = FakePeerBeam()
      ..logLines = [
        _line('INFO', 'engine started'),
        _line('INFO', 'discovery started'),
        _line('WARN', 'peer unreachable'),
      ];
    await _open(tester, fake);

    expect(fake.calls, contains('logs:200'));
    expect(
      tester.getTopLeft(find.text('peer unreachable')).dy,
      greaterThan(tester.getTopLeft(find.text('engine started')).dy),
      reason: 'the newest line belongs at the bottom, where the log ends',
    );
  });

  /// **A problem is visible without reading.** The icon and the container
  /// colour come off `isProblem`, which is the engine's answer — a UI that
  /// matched on level strings would silently miss a level it had not heard of.
  testWidgets('warnings and errors are marked apart from ordinary lines', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..logLines = [
        _line('INFO', 'engine started'),
        _line('WARN', 'peer unreachable'),
        _line('ERROR', 'transfer failed'),
      ];
    await _open(tester, fake);

    expect(find.byIcon(Icons.error_outline_rounded), findsNWidgets(2));
    expect(find.byIcon(Icons.info_outline_rounded), findsOneWidget);
  });

  /// The metadata line: time-of-day, level and target, with the date dropped
  /// because the buffer only ever covers the running session.
  testWidgets('each line carries its time, level and target', (tester) async {
    final fake = FakePeerBeam()..logLines = [_line('WARN', 'peer unreachable')];
    await _open(tester, fake);

    expect(find.text('10:04:12 · WARN · pb'), findsOneWidget);
  });

  /// **Export reports where the file went.** A bug report needs the path, and
  /// the snackbar is the only place it is ever said.
  testWidgets('exporting reports the path it wrote to', (tester) async {
    final fake = FakePeerBeam()..logLines = [_line('INFO', 'engine started')];
    await _open(tester, fake);

    await tester.tap(find.byTooltip('Export to a file'));
    await tester.pumpAndSettle();

    expect(fake.calls, contains('exportLogs:'));
    expect(
      find.text('Logs written to /tmp/peerbeam-logs.jsonl'),
      findsOneWidget,
      reason:
          'naming the file is the difference between an export and a rumour',
    );
    expect(find.text('Copy path'), findsOneWidget);
  });

  /// An empty buffer is still exportable: "nothing happened" is itself an
  /// answer a bug report may need.
  testWidgets('an empty buffer can still be exported', (tester) async {
    await _open(tester, FakePeerBeam());

    await tester.tap(find.byTooltip('Export to a file'));
    await tester.pumpAndSettle();

    expect(find.textContaining('Logs written to'), findsOneWidget);
  });

  /// Reachable from Settings — the SDK has had these calls all along, and the
  /// gap this closes is that nothing in the app made them.
  testWidgets('Settings opens the log screen', (tester) async {
    final fake = FakePeerBeam()..logLines = [_line('INFO', 'engine started')];
    final state = AppState.live(fake);
    addTearDown(state.dispose);
    await tester.pumpWidget(
      AppScope(
        state: state,
        child: const MaterialApp(home: SettingsScreen()),
      ),
    );
    await tester.pump();
    await state.settings.load(fake);
    await tester.pumpAndSettle();
    await tester.scrollUntilVisible(
      find.text('DIAGNOSTICS'),
      300,
      scrollable: find.byType(Scrollable).first,
    );
    await tester.pumpAndSettle();

    await tester.tap(find.text('Logs'));
    await tester.pumpAndSettle();

    expect(find.text('engine started'), findsOneWidget);
    expect(fake.calls, contains('logs:200'));
  });
}
