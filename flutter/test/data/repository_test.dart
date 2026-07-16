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

    test('fileReceived carries the path, name, and sending peer', () async {
      final fake = FakePeerBeam();
      final repo = TransferRepository(api: fake);
      final received = <({String path, String name, String peer})>[];
      repo.fileReceived.listen(received.add);

      fake.emit(
        ev('transfer_queued', 't2', {
          'peer': 'Alice',
          'file': 'movie.mkv',
          'incoming': true,
        }),
      );
      await flush();
      fake.emit(ev('transfer_completed', 't2', {'path': '/data/movie.mkv'}));
      await flush();

      expect(received, hasLength(1));
      expect(received.single.path, '/data/movie.mkv');
      expect(received.single.name, 'movie.mkv');
      expect(received.single.peer, 'Alice');
    });

    test(
      'a progress heartbeat after pause stays paused, not transferring',
      () async {
        // Regression test: `transfer_progress` used to unconditionally set
        // state to `transferring`, so the engine's ~1s progress heartbeats
        // flipped a paused transfer back to "transferring" in the UI even
        // though nothing was moving — defeating pause. `transfer_paused` must
        // stick until an explicit `transfer_resumed`.
        final fake = FakePeerBeam();
        final repo = TransferRepository(api: fake);

        fake.emit(
          ev('transfer_queued', 't3', {'peer': 'Bob', 'file': 'a.bin'}),
        );
        await flush();
        fake.emit(
          ev('transfer_progress', 't3', {
            'stats': {'transferred_bytes': 10, 'total_bytes': 100},
          }),
        );
        await flush();
        expect(repo.transfers.single.state, ui.TransferState.transferring);

        fake.emit(ev('transfer_paused', 't3'));
        await flush();
        expect(repo.transfers.single.state, ui.TransferState.paused);

        // A heartbeat lands while still paused — must not flip back.
        fake.emit(
          ev('transfer_progress', 't3', {
            'stats': {
              'transferred_bytes': 10,
              'total_bytes': 100,
              'current_speed': 999,
              'eta_secs': 5,
            },
          }),
        );
        await flush();
        final paused = repo.transfers.single;
        expect(paused.state, ui.TransferState.paused);
        expect(paused.speedBps, 0);
        expect(paused.etaSecs, isNull);

        // Resume: the next progress heartbeat goes back to transferring.
        fake.emit(ev('transfer_resumed', 't3'));
        await flush();
        expect(repo.transfers.single.state, ui.TransferState.transferring);
        fake.emit(
          ev('transfer_progress', 't3', {
            'stats': {
              'transferred_bytes': 20,
              'total_bytes': 100,
              'current_speed': 42,
              'eta_secs': 3,
            },
          }),
        );
        await flush();
        final resumed = repo.transfers.single;
        expect(resumed.state, ui.TransferState.transferring);
        expect(resumed.speedBps, 42);
        expect(resumed.etaSecs, 3);
      },
    );

    test('commands delegate to the engine', () async {
      final fake = FakePeerBeam();
      final repo = TransferRepository(api: fake);
      repo.pause('t1');
      repo.resume('t1');
      repo.cancel('t1');
      await flush();
      expect(fake.calls, containsAll(['pause:t1', 'resume:t1', 'cancel:t1']));
    });

    test(
      'a queued folder send is labeled with the folder name, not blank',
      () async {
        // Regression test: send_folder()'s transfer_queued payload carries
        // `folder`, not `file` (rust transfer.rs). Without a fallback,
        // Transfer.fileName was '' until the first per-file progress event.
        final fake = FakePeerBeam();
        final repo = TransferRepository(api: fake);

        fake.emit(
          ev('transfer_queued', 't5', {'peer': 'Bob', 'folder': 'Photos'}),
        );
        await flush();
        expect(repo.transfers.single.fileName, 'Photos');
      },
    );

    test('progress is clamped even if reported done exceeds total', () async {
      // Regression test: Transfer.progress used to be unclamped, unlike its
      // SDK twin TransferStats.progress, so a transient done > total could
      // render as e.g. "103%".
      final fake = FakePeerBeam();
      final repo = TransferRepository(api: fake);

      fake.emit(ev('transfer_queued', 't6', {'peer': 'Bob', 'file': 'a.bin'}));
      await flush();
      fake.emit(
        ev('transfer_progress', 't6', {
          'stats': {'transferred_bytes': 150, 'total_bytes': 100},
        }),
      );
      await flush();
      expect(repo.transfers.single.progress, 1.0);
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
      // No longer refreshes in the constructor (that runs before the engine
      // is initialized in production); callers must refresh explicitly.
      await repo.refresh();
      await flush();
      expect(repo.items.single.id, 'h1');

      fake.historyEntries = [];
      fake.emit(const HistoryUpdated());
      await flush();
      expect(repo.items, isEmpty);
    });
  });
}
