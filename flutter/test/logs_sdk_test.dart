import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/sdk/models.dart';

import 'sdk/fake_peerbeam.dart';

void main() {
  test('a log line decodes every field the engine records', () {
    final l = LogLine.fromJson(const {
      'at': '2026-08-19T10:00:00Z',
      'level': 'WARN',
      'target': 'peerbeam_sync',
      'message': 'peer unreachable',
    });
    expect(l.at, '2026-08-19T10:00:00Z');
    expect(l.level, 'WARN');
    expect(l.target, 'peerbeam_sync');
    expect(l.message, 'peer unreachable');
  });

  /// A log is opened because something went wrong, so the line that says so has
  /// to be identifiable without the reader parsing level strings themselves.
  test('warnings and errors are flagged as problems, info is not', () {
    for (final level in ['ERROR', 'WARN']) {
      expect(
        LogLine.fromJson({'level': level}).isProblem,
        isTrue,
        reason: '$level should read as a problem',
      );
    }
    for (final level in ['INFO', 'DEBUG', 'TRACE', '']) {
      expect(LogLine.fromJson({'level': level}).isProblem, isFalse);
    }
  });

  test('a line missing fields decodes rather than throwing', () {
    // An older engine, or a line the layer could not fully describe.
    final l = LogLine.fromJson(const {'message': 'bare'});
    expect(l.message, 'bare');
    expect(l.level, '');
    expect(l.at, '');
  });

  test('logs are reachable from the SDK and bounded by the limit', () async {
    final api = FakePeerBeam();
    api.logLines = List.generate(
      10,
      (i) => LogLine(at: '', level: 'INFO', target: 't', message: '$i'),
    );

    final all = await api.logs();
    expect(all, hasLength(10));

    final tail = await api.logs(limit: 3);
    expect(tail, hasLength(3));
    expect(tail.last.message, '9', reason: 'the newest lines must survive');
    expect(api.calls, contains('logs:3'));
  });

  test('exporting logs returns where they went', () async {
    final api = FakePeerBeam();
    expect(await api.exportLogs(path: '/tmp/x.jsonl'), '/tmp/x.jsonl');
    expect(await api.exportLogs(), isNotEmpty);
  });

  test(
    'log streaming is off until switched on, and can be switched off',
    () async {
      final api = FakePeerBeam();
      expect(api.logsSubscribed, isFalse);
      await api.subscribeLogs(true);
      expect(api.logsSubscribed, isTrue);
      await api.subscribeLogs(false);
      expect(api.logsSubscribed, isFalse);
    },
  );
}
