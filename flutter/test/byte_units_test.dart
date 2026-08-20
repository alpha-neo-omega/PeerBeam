// One byte count, one answer, everywhere it is shown.
//
// The app used to hold three copies of this formatter: the shared one in
// `state/models.dart` divided by 1024, and the devices dashboard and the
// auto-save rules each carried their own dividing by 1000 — the dashboard's
// copy shadowing the import inside its own library. All three printed
// `KB`/`MB`/`GB`. So 1 GiB of staged files read "1.0 GB" in Home's selection
// bar and "1.1 GB" on the dashboard, and the shared copy's label was not true
// to its own arithmetic.
//
// Two properties these tests pin, and the second is the one that keeps the
// first from rotting:
//
//  1. **The divisor matches the label.** `KB` means 1000 bytes, because that is
//     what the CLI's `human_bytes` prints for the identical presence status.
//  2. **Surfaces call the shared formatter.** Asserting a surface renders
//     exactly `formatBytes(n)` — rather than a hardcoded string — is what fails
//     if anyone reintroduces a local copy that disagrees.
//
// Both libraries below are imported unprefixed on purpose. A second public
// `formatBytes` on the devices screen would make this file ambiguous and stop
// it compiling, which catches the exact shape the defect had; a private copy
// slips past that and is caught by the surface assertions instead.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/devices/devices_screen.dart';
import 'package:peerbeam/features/settings/settings_screen.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/models.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

const _gibibyte = 1024 * 1024 * 1024;

Device _device(String id) => Device(
  id: id,
  name: 'Bob',
  kind: DeviceKind.desktop,
  online: true,
  reach: const {Reach.lan},
);

void main() {
  group('formatBytes', () {
    test('KB means 1000 bytes, the unit it prints', () {
      // The exact powers of ten, so the boundary a decimal formatter turns on
      // is the boundary its label claims.
      expect(formatBytes(999), '999 B');
      expect(formatBytes(1000), '1.0 KB');
      expect(formatBytes(1000 * 1000), '1.0 MB');
      expect(formatBytes(1000 * 1000 * 1000), '1.0 GB');

      // And the mismatch that started this: a binary gigabyte is not "1.0 GB".
      // Whichever way the convention had gone, these two lines had to differ —
      // the old formatter printed 1.0 for both.
      expect(formatBytes(_gibibyte), '1.1 GB');
      expect(formatBytes(1024), '1.0 KB');
    });

    test('a speed carries the same units as the size it derives from', () {
      // `formatSpeed` delegates, so a second convention here would be a third
      // answer for the same number.
      expect(formatSpeed(1000 * 1000), '1.0 MB/s');
      expect(formatSpeed(0), '');
    });
  });

  /// The dashboard's chip is the surface whose own copy disagreed with the
  /// import it shadowed. Compared against `formatBytes` rather than a literal:
  /// this fails for any local copy, not just the one that was there.
  testWidgets('the devices dashboard renders storage via formatBytes', (
    tester,
  ) async {
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: DeviceStatusCard(
            device: _device('pb-bob'),
            presence: const SdkPresence(
              deviceId: 'pb-bob',
              batteryPercent: null,
              charging: null,
              storageFreeBytes: _gibibyte,
              network: null,
              appVersion: null,
              ageSeconds: 3,
            ),
          ),
        ),
      ),
    );

    expect(find.text('${formatBytes(_gibibyte)} free'), findsOneWidget);
  });

  /// The auto-save rules row is the third copy. A size criterion is the number
  /// a user typed in bytes, so a row that rounds it by a different rule than
  /// the rest of the app misreports the rule they are looking at.
  testWidgets('a size criterion is rendered via formatBytes', (tester) async {
    final fake = FakePeerBeam()
      ..settings.addAll({
        'rules_supported': true,
        'save_rules': [
          {
            'min_bytes': _gibibyte,
            'max_bytes': 10 * _gibibyte,
            'directory': '/srv/big',
          },
        ],
      });
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
    await tester.pump();
    await tester.scrollUntilVisible(
      find.text('AUTO-SAVE RULES'),
      300,
      scrollable: find.byType(Scrollable).first,
    );
    await tester.pumpAndSettle();

    expect(
      find.text('${formatBytes(_gibibyte)}–${formatBytes(10 * _gibibyte)}'),
      findsOneWidget,
    );
  });
}
