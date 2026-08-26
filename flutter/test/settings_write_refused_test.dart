// A refused settings write must not leave the screen claiming it succeeded.
//
// The store used to fire and forget: `unawaited(api.settingsSet(..)
// .catchError((_) {}))`. The switch moved, the store reported the new value, and
// nothing learned the engine had refused. For most settings that is a stale
// preference. For two of them it is a false assurance about the user's security
// posture — being told clipboard contents are no longer leaving this device, or
// that new devices no longer need a pairing code, when neither is true.

import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

/// A store with the fields the constructor requires; the values are irrelevant
/// to what these tests assert.
SettingsStore _store() => SettingsStore(
  deviceName: 'This Device',
  saveDirectory: '/tmp',
  autoAcceptTrusted: false,
  notifications: true,
  compression: true,
);

void main() {
  test('a refused write reverts the value and reports the failure', () async {
    final fake = FakePeerBeam();
    final settings = _store();
    // Loaded first, then made to fail: `load` must succeed or the store has no
    // engine attached and every write is a silent no-op.
    await settings.load(fake);
    fake.failing.add('settingsSet');
    final before = settings.syncClipboard;

    await expectLater(
      settings.setSyncClipboard(!before),
      throwsA(anything),
      reason: 'the caller must be able to tell the user',
    );
    expect(
      settings.syncClipboard,
      before,
      reason: 'the store kept a value the engine refused',
    );
  });

  /// **The two writes that are not switches said nothing when refused.** Every
  /// toggle on that screen runs through `_guardedSwitch`, which catches the
  /// rethrow and names what failed; the device name and the save directory
  /// awaited it bare, so the value reverted and the screen stayed silent — which
  /// reads as the app having ignored the change rather than refused it.
  ///
  /// Asserted on the store, which is where the rethrow comes from: the screen
  /// cannot report a failure that never reaches it.
  test('the device name and save directory rethrow when refused', () async {
    final fake = FakePeerBeam();
    final settings = _store();
    await settings.load(fake);
    fake.failing.add('settingsSet');

    final name = settings.deviceName;
    await expectLater(settings.setDeviceName('Renamed'), throwsA(anything));
    expect(settings.deviceName, name, reason: 'a refused rename was kept');

    final dir = settings.saveDirectory;
    await expectLater(
      settings.setSaveDirectory('/tmp/elsewhere'),
      throwsA(anything),
    );
    expect(settings.saveDirectory, dir, reason: 'a refused path was kept');
  });

  test('a write that succeeds is kept and reaches the engine', () async {
    final fake = FakePeerBeam();
    final settings = _store();
    await settings.load(fake);

    await settings.setSyncClipboard(true);
    expect(settings.syncClipboard, isTrue);
    expect(fake.settings['sync_clipboard'], isTrue);
  });

  test('the security-relevant settings revert too', () async {
    // Named individually because these two are the ones where silence is a
    // false assurance rather than a stale preference.
    for (final probe in [
      (
        'require_pairing_confirmation',
        (SettingsStore s) => s.setRequirePairingConfirmation(false),
        (SettingsStore s) => s.requirePairingConfirmation,
      ),
      (
        'sync_clipboard',
        (SettingsStore s) => s.setSyncClipboard(true),
        (SettingsStore s) => s.syncClipboard,
      ),
    ]) {
      final fake = FakePeerBeam()..failing.add('settingsSet');
      final settings = _store();
      await settings.load(fake);
      final before = probe.$3(settings);

      await probe.$2(settings).then((_) => null, onError: (_) => null);
      expect(
        probe.$3(settings),
        before,
        reason: '${probe.$1} kept a refused value',
      );
    }
  });

  test('notifying happens for both the apply and the revert', () async {
    final fake = FakePeerBeam();
    final settings = _store();
    // Loaded first, then made to fail: `load` must succeed or the store has no
    // engine attached and every write is a silent no-op.
    await settings.load(fake);
    fake.failing.add('settingsSet');
    var notifications = 0;
    settings.addListener(() => notifications++);

    await settings
        .setNotifications(false)
        .then((_) => null, onError: (_) => null);
    expect(
      notifications,
      greaterThanOrEqualTo(2),
      reason: 'the revert must rebuild the control, or it shows the old choice',
    );
  });
}
