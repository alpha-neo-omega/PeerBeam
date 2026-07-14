// Repository tests over a mock SDK — no native library. Prove repositories are
// event-driven (state updates from engine events) and delegate commands to the
// SDK (no transfer logic in Dart).

import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/data/discovery_repository.dart';
import 'package:peerbeam/data/history_repository.dart';
import 'package:peerbeam/data/transfer_repository.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/models.dart' as ui;

import '../sdk/fake_peerbeam.dart';

/// Flush pending microtasks so stream listeners run.
Future<void> flush() => Future(() {});

SdkDevice dev(String id, {bool online = true}) => SdkDevice(
  id: id,
  name: 'Dev $id',
  kind: 'laptop',
  platform: 'linux',
  addresses: const ['127.0.0.1'],
  port: 49600,
  online: online,
  latencyMs: 5,
  reachableLan: true,
  reachableRemote: false,
);

void main() {
  group('DiscoveryRepository', () {
    test('adds/updates/removes devices from events', () async {
      final fake = FakePeerBeam();
      final repo = DiscoveryRepository(api: fake);

      fake.emit(DeviceAdded(dev('a')));
      await flush();
      expect(repo.devices.map((d) => d.id), ['a']);
      expect(repo.onlineCount, 1);

      fake.emit(const DeviceStatusChanged('a', false));
      await flush();
      expect(repo.devices.single.online, isFalse);
      expect(repo.onlineCount, 0);

      fake.emit(const DeviceRemoved('a'));
      await flush();
      expect(repo.devices, isEmpty);
    });

    test('toggleScan delegates to the engine', () async {
      final fake = FakePeerBeam();
      final repo = DiscoveryRepository(api: fake);
      repo.toggleScan();
      await flush();
      expect(fake.calls, contains('start'));
      repo.toggleScan();
      await flush();
      expect(fake.calls, contains('stop'));
    });
  });

  group('TransferRepository', () {
    TransferEvent ev(String kind, String id, [Map<String, dynamic>? p]) =>
        TransferEvent(
          kind: kind,
          transferId: id,
          timestamp: '',
          payload: p ?? {},
        );

    test('builds and updates a transfer from its event sequence', () async {
      final fake = FakePeerBeam();
      final repo = TransferRepository(api: fake);

      fake.emit(ev('transfer_queued', 't1', {'peer': 'Bob', 'file': 'a.bin'}));
      await flush();
      expect(repo.transfers.single.id, 't1');
      expect(repo.transfers.single.state, ui.TransferState.pending);

      fake.emit(
        ev('transfer_progress', 't1', {
          'stats': {'transferred_bytes': 50, 'total_bytes': 100},
          'file': 'a.bin',
        }),
      );
      await flush();
      expect(repo.transfers.single.doneBytes, 50);
      expect(repo.transfers.single.totalBytes, 100);
      expect(repo.activeCount, 1);

      fake.emit(ev('transfer_completed', 't1'));
      await flush();
      expect(repo.transfers, isEmpty); // moves out of active
    });

    test('commands delegate to the engine', () async {
      final fake = FakePeerBeam();
      final repo = TransferRepository(api: fake);
      repo.pause('t1');
      repo.resume('t1');
      repo.cancel('t1');
      await flush();
      expect(fake.calls, containsAll(['pause:t1', 'resume:t1', 'cancel:t1']));
    });
  });

  group('HistoryRepository', () {
    test('refreshes from the engine on history_updated', () async {
      final fake = FakePeerBeam()
        ..historyEntries = [
          const HistoryEntry(
            id: 'h1',
            direction: 'sending',
            peer: 'Bob',
            file: 'a.bin',
            path: '/tmp/a.bin',
            bytes: 100,
            success: true,
            at: '2026-01-01T00:00:00Z',
          ),
        ];
      final repo = HistoryRepository(api: fake);
      await flush();
      // Initial refresh in the constructor.
      expect(repo.items.single.id, 'h1');

      fake.historyEntries = [];
      fake.emit(const HistoryUpdated());
      await flush();
      expect(repo.items, isEmpty);
    });
  });
}
