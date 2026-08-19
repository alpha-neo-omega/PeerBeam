// A dead engine must not look like an idle network.
//
// The whole boot sequence lives inside one `catch (_) {}`. When `initialize()`
// failed, every screen fell back to its calm empty state — "No nearby devices.
// Devices on your network appear here." — and the user went to check their
// router for a problem that never reached the router.
//
// The honest copy already existed in `sdk/error_text.dart`; nothing carried the
// failure to it.

import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/state/stores.dart';

void main() {
  test('a fresh status is booting, neither ready nor failed', () {
    final status = EngineStatus();
    expect(status.booting, isTrue);
    expect(status.ready, isFalse);
    expect(status.failure, isNull);
  });

  test('a started engine is ready', () {
    final status = EngineStatus()..started();
    expect(status.ready, isTrue);
    expect(status.failure, isNull);
  });

  test('a failed engine keeps the reason rather than logging it away', () {
    final status = EngineStatus()..failed(StateError('no native library'));
    expect(status.ready, isFalse);
    expect(status.booting, isFalse);
    expect(
      status.failure.toString(),
      contains('no native library'),
      reason: 'the reason is what the user is shown; losing it is the bug',
    );
  });

  test('the status notifies, or the banner never appears', () {
    var notified = 0;
    final status = EngineStatus()..addListener(() => notified++);
    status.failed(StateError('boom'));
    expect(notified, 1);
    status.started();
    expect(notified, 2);
  });

  test('recovering clears the failure', () {
    final status = EngineStatus()..failed(StateError('boom'));
    status.started();
    expect(status.failure, isNull);
    expect(status.ready, isTrue);
  });
}
