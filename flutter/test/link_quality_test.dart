// Link quality: the round-trip time the transport measured for a device, from
// the engine event that carries it to the chip that renders it.
//
// The property worth defending is *provenance*. A shared battery level is
// something a peer chose to tell us; a round trip is something this device
// measured about its own connection. They arrive by different routes, they are
// absent for different reasons, and a card that blurred them would let one
// device's silence hide the other fact — or worse, let a figure from an
// hour-old connection read as a live one after the engine cleared it.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/data/discovery_repository.dart';
import 'package:peerbeam/features/devices/devices_screen.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/models.dart';

import 'sdk/fake_peerbeam.dart';

SdkDevice _sdkDevice(String id, {int? latencyMs}) => SdkDevice(
  id: id,
  name: 'Bob',
  kind: 'desktop',
  platform: 'linux',
  addresses: const ['10.0.0.5'],
  port: 9000,
  online: true,
  latencyMs: latencyMs,
  reachableLan: true,
  reachableRemote: false,
);

Device _device(String id, {int? latencyMs}) => Device(
  id: id,
  name: 'Bob',
  kind: DeviceKind.desktop,
  online: true,
  reach: const {Reach.lan},
  latencyMs: latencyMs,
);

SdkPresence _presence(String id, {int? battery}) => SdkPresence(
  deviceId: id,
  batteryPercent: battery,
  charging: null,
  storageFreeBytes: null,
  network: null,
  appVersion: null,
  ageSeconds: 3,
);

Widget _card(Device d, [SdkPresence? p]) => MaterialApp(
  home: Scaffold(
    body: DeviceStatusCard(device: d, presence: p),
  ),
);

void main() {
  group('formatLatency', () {
    test('a zero is a very fast link, not a missing one', () {
      // The engine expresses "not measured" with null. A rounded-down zero is
      // a real reading, so it must not borrow null's rendering.
      expect(formatLatency(0), '<1 ms');
      expect(formatLatency(1), '1 ms');
      expect(formatLatency(237), '237 ms');
    });
  });

  group('DiscoveryRepository', () {
    test('a measured round trip lands on the device it names', () async {
      final fake = FakePeerBeam();
      final repo = DiscoveryRepository(api: fake);
      addTearDown(repo.dispose);

      fake.emit(DeviceAdded(_sdkDevice('pb-bob')));
      fake.emit(const DeviceLatencyChanged('pb-bob', 12));
      await Future<void>.delayed(Duration.zero);

      expect(repo.devices.single.latencyMs, 12);
    });

    test(
      'a cleared measurement clears, rather than leaving the old one up',
      () async {
        // The engine sends `latency_ms: null` when it holds a live link it
        // cannot characterise. Keeping the previous figure would present a
        // number from an earlier connection as if it described this one — and a
        // `latencyMs ?? this.latencyMs` copy did exactly that, silently.
        final fake = FakePeerBeam();
        final repo = DiscoveryRepository(api: fake);
        addTearDown(repo.dispose);

        fake.emit(DeviceAdded(_sdkDevice('pb-bob')));
        fake.emit(const DeviceLatencyChanged('pb-bob', 12));
        await Future<void>.delayed(Duration.zero);
        fake.emit(const DeviceLatencyChanged('pb-bob', null));
        await Future<void>.delayed(Duration.zero);

        expect(repo.devices.single.latencyMs, isNull);
      },
    );

    test(
      'going offline does not discard the measurement of other fields',
      () async {
        // `copyWith`'s sentinel must leave latency alone when only `online`
        // moves — the whole reason the method exists is that update paths used
        // to drop fields they forgot to re-list.
        final fake = FakePeerBeam();
        final repo = DiscoveryRepository(api: fake);
        addTearDown(repo.dispose);

        fake.emit(DeviceAdded(_sdkDevice('pb-bob', latencyMs: 8)));
        fake.emit(const DeviceStatusChanged('pb-bob', false));
        await Future<void>.delayed(Duration.zero);

        expect(repo.devices.single.online, isFalse);
        expect(repo.devices.single.latencyMs, 8);
      },
    );
  });

  group('DeviceStatusCard', () {
    testWidgets('a measured round trip is labelled, not a bare number', (
      tester,
    ) async {
      await tester.pumpWidget(_card(_device('pb-bob', latencyMs: 12)));

      expect(find.text('12 ms round trip'), findsOneWidget);
    });

    testWidgets(
      'a device that shares nothing still shows the link we measured',
      (tester) async {
        // The provenance split: the peer disclosed nothing, and this figure was
        // never the peer's to disclose.
        await tester.pumpWidget(_card(_device('pb-bob', latencyMs: 12)));

        expect(find.text('12 ms round trip'), findsOneWidget);
        expect(find.text('Status not shared'), findsOneWidget);
      },
    );

    testWidgets(
      'a device we have not connected to shows no round trip at all',
      (tester) async {
        // Not "0 ms", not "—". A link nobody measured has nothing to say.
        await tester.pumpWidget(
          _card(_device('pb-bob'), _presence('pb-bob', battery: 64)),
        );

        expect(find.textContaining('round trip'), findsNothing);
        expect(find.text('64%'), findsOneWidget);
      },
    );

    testWidgets('a sub-millisecond link reads as under a millisecond', (
      tester,
    ) async {
      await tester.pumpWidget(_card(_device('pb-bob', latencyMs: 0)));

      expect(find.text('<1 ms round trip'), findsOneWidget);
      expect(find.textContaining('0 ms'), findsNothing);
    });

    testWidgets('the measured link and the shared status sit side by side', (
      tester,
    ) async {
      await tester.pumpWidget(
        _card(
          _device('pb-bob', latencyMs: 4),
          _presence('pb-bob', battery: 64),
        ),
      );

      expect(find.text('4 ms round trip'), findsOneWidget);
      expect(find.text('64%'), findsOneWidget);
      expect(find.text('Status not shared'), findsNothing);
    });
  });
}
