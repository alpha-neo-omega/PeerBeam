// Presence: the repository that reduces heartbeat events into a live view, and
// the card that renders one.
//
// The property worth defending here is *honesty about absence*. A device that
// shares nothing, a field a device could not measure, and a device whose status
// has not arrived yet are all different from a zero — and a dashboard that
// renders any of them as `0%` or `0 B free` is inventing a dead battery and a
// full disk that were never measured. Every test below exists to keep a null
// looking like a null.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/data/presence_repository.dart';
import 'package:peerbeam/features/devices/devices_screen.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/models.dart';

import 'sdk/fake_peerbeam.dart';

SdkPresence _presence(
  String id, {
  int? battery,
  bool? charging,
  int? storage,
  String? network,
  String? version,
  int age = 3,
}) => SdkPresence(
  deviceId: id,
  batteryPercent: battery,
  charging: charging,
  storageFreeBytes: storage,
  network: network,
  appVersion: version,
  ageSeconds: age,
);

Device _device(String id, {String name = 'Bob'}) => Device(
  id: id,
  name: name,
  kind: DeviceKind.desktop,
  online: true,
  reach: const {Reach.lan},
);

Widget _card(Device d, SdkPresence? p) => MaterialApp(
  home: Scaffold(
    body: DeviceStatusCard(device: d, presence: p),
  ),
);

void main() {
  group('PresenceRepository', () {
    test('a heartbeat lands under its own device id', () async {
      final fake = FakePeerBeam();
      final repo = PresenceRepository(api: fake);
      addTearDown(repo.dispose);

      fake.emit(PresenceUpdated(_presence('pb-bob', battery: 82)));
      await Future<void>.delayed(Duration.zero);

      expect(repo.of('pb-bob')?.batteryPercent, 82);
      expect(repo.of('pb-alice'), isNull, reason: 'no cross-contamination');
      expect(repo.sharedCount, 1);
    });

    test('a later heartbeat replaces the peer, never accumulates', () async {
      final fake = FakePeerBeam();
      final repo = PresenceRepository(api: fake);
      addTearDown(repo.dispose);

      fake.emit(PresenceUpdated(_presence('pb-bob', battery: 82)));
      await Future<void>.delayed(Duration.zero);
      fake.emit(PresenceUpdated(_presence('pb-bob', battery: 41)));
      await Future<void>.delayed(Duration.zero);

      expect(repo.sharedCount, 1);
      expect(repo.of('pb-bob')?.batteryPercent, 41);
    });

    test('a failed refresh keeps the view it already had', () async {
      // A dashboard that is still accurate must not be blanked by a transient
      // engine error — that would read as "everyone went offline".
      final fake = FakePeerBeam();
      final repo = PresenceRepository(api: fake);
      addTearDown(repo.dispose);
      fake.emit(PresenceUpdated(_presence('pb-bob', battery: 82)));
      await Future<void>.delayed(Duration.zero);

      fake.presenceThrows = true;
      await repo.refresh();

      expect(repo.of('pb-bob')?.batteryPercent, 82);
    });

    test('refresh adopts the engine snapshot, including sharing', () async {
      final fake = FakePeerBeam()
        ..presenceSnapshot = PresenceSnapshot(
          sharing: true,
          self: _presence('me', battery: 55),
          devices: {'pb-bob': _presence('pb-bob', storage: 1024)},
        );
      final repo = PresenceRepository(api: fake);
      addTearDown(repo.dispose);

      await repo.refresh();

      expect(repo.sharing, isTrue);
      expect(repo.self?.batteryPercent, 55);
      expect(repo.of('pb-bob')?.storageFreeBytes, 1024);
    });

    test('sharing defaults to off before anything is fetched', () {
      final repo = PresenceRepository(api: FakePeerBeam());
      addTearDown(repo.dispose);
      expect(repo.sharing, isFalse);
    });
  });

  group('DeviceStatusCard', () {
    testWidgets('a device sharing nothing shows identity, not empty gauges', (
      tester,
    ) async {
      await tester.pumpWidget(_card(_device('pb-bob'), null));

      expect(find.text('Bob'), findsOneWidget);
      expect(find.text('Status not shared'), findsOneWidget);
      // The whole point: no invented readings.
      expect(find.textContaining('%'), findsNothing);
      expect(find.textContaining('free'), findsNothing);
    });

    testWidgets('only the fields a device actually shared are rendered', (
      tester,
    ) async {
      // Battery only. A desktop reports no battery at all, and Windows/macOS
      // report none by design — so a card must not imply the missing ones are
      // zero.
      await tester.pumpWidget(
        _card(_device('pb-bob'), _presence('pb-bob', battery: 64)),
      );

      expect(find.text('64%'), findsOneWidget);
      expect(find.textContaining('free'), findsNothing);
      expect(find.text('Status not shared'), findsNothing);
    });

    testWidgets('a shared status with no battery renders no battery at all', (
      tester,
    ) async {
      // The case the "shares nothing" test cannot reach: this device DID send
      // a status, it simply has no battery to report — every desktop, and
      // every Windows/macOS build, where the collector is omitted by design.
      // A card that filled that in as 0% would invent a dead battery on the
      // most common device in the fleet.
      await tester.pumpWidget(
        _card(
          _device('pb-bob'),
          _presence('pb-bob', storage: 1024, network: 'ethernet'),
        ),
      );

      expect(find.textContaining('%'), findsNothing);
      expect(find.textContaining('free'), findsOneWidget);
    });

    testWidgets('charging is stated, not just implied by the icon', (
      tester,
    ) async {
      await tester.pumpWidget(
        _card(
          _device('pb-bob'),
          _presence('pb-bob', battery: 64, charging: true),
        ),
      );

      expect(find.text('64% charging'), findsOneWidget);
    });

    testWidgets('storage and network render when shared', (tester) async {
      await tester.pumpWidget(
        _card(
          _device('pb-bob'),
          _presence('pb-bob', storage: 5 * 1024 * 1024 * 1024, network: 'wifi'),
        ),
      );

      expect(find.textContaining('free'), findsOneWidget);
      expect(find.text('Wi-Fi'), findsOneWidget);
    });

    testWidgets('a zero battery is shown, because zero is a real reading', (
      tester,
    ) async {
      // The mirror image of the tests above: absent must not render as zero,
      // and zero must not be swallowed as absent.
      await tester.pumpWidget(
        _card(_device('pb-bob'), _presence('pb-bob', battery: 0)),
      );

      expect(find.text('0%'), findsOneWidget);
      expect(find.text('Status not shared'), findsNothing);
    });
  });
}
