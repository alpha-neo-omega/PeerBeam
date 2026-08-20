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

LogLine _line(String level, String message, {String at = _utcStamp}) =>
    LogLine(at: at, level: level, target: 'pb', message: message);

/// What `peerbeam-logs` writes: `Utc::now().to_rfc3339()`.
const _utcStamp = '2026-08-19T10:04:12.123Z';

/// `at` on this machine's clock, `HH:mm:ss` — computed rather than written out,
/// so the expectation is right in every timezone a contributor runs the suite
/// in (including UTC, where it happens to equal the stamp's own digits).
String _localClock(String at) {
  final local = DateTime.parse(at).toLocal();
  String two(int n) => n.toString().padLeft(2, '0');
  return '${two(local.hour)}:${two(local.minute)}:${two(local.second)}';
}

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

    expect(find.text('${_localClock(_utcStamp)} · WARN · pb'), findsOneWidget);
  });

  /// **The time is this device's, not the engine's.** The engine stamps every
  /// line in UTC and the screen used to slice the characters after the `T`
  /// straight out, so a machine in Kolkata showed 10:04 for a line written at
  /// 15:34 on its own wall clock — five and a half hours of drift, unlabelled,
  /// on the one screen whose whole job is matching "it broke just now" to a
  /// line.
  ///
  /// Two offsets, because one is not enough to prove anything: whichever zone
  /// the suite runs in, at most one of these can accidentally agree with the
  /// stamp's own digits, so the naive substring cannot pass both.
  testWidgets('a stamp is shown on this device\'s clock, not the engine\'s', (
    tester,
  ) async {
    const ahead = '2026-08-19T10:04:12.123+05:30';
    const behind = '2026-08-19T10:04:12.123-07:00';
    final fake = FakePeerBeam()
      ..logLines = [
        _line('INFO', 'from a machine ahead of us', at: ahead),
        _line('INFO', 'from a machine behind us', at: behind),
      ];
    await _open(tester, fake);

    expect(find.text('${_localClock(ahead)} · INFO · pb'), findsOneWidget);
    expect(find.text('${_localClock(behind)} · INFO · pb'), findsOneWidget);
  });

  /// The other half of that conversion: anything that is not a date *and* a
  /// time is still shown exactly as it came. A bare date would otherwise parse
  /// and render as `00:00:00` — a time nothing happened at — and a string that
  /// is not a stamp at all has no shape to guess.
  testWidgets('a stamp the screen cannot read is shown untouched', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..logLines = [
        _line('INFO', 'no stamp', at: 'startup'),
        _line('INFO', 'date only', at: '2026-08-19'),
      ];
    await _open(tester, fake);

    expect(find.text('startup · INFO · pb'), findsOneWidget);
    expect(find.text('2026-08-19 · INFO · pb'), findsOneWidget);
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
