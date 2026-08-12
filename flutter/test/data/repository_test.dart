// Repository tests over a mock SDK — no native library. Prove repositories are
// event-driven (state updates from engine events) and delegate commands to the
// SDK (no transfer logic in Dart).

import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/data/chat_repository.dart';
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

  group('ChatRepository', () {
    test('refresh pulls the conversation from the engine', () async {
      final fake = FakePeerBeam();
      fake.chatHistories['bob'] = [
        ChatMessage(
          id: 'm1',
          peerId: 'bob',
          direction: 'in',
          body: 'hi',
          at: DateTime.now(),
          status: 'received',
        ),
      ];
      final repo = ChatRepository(api: fake);
      // No refresh in the constructor (same reasoning as the other
      // repositories); callers refresh explicitly once a conversation opens.
      expect(repo.messagesFor('bob'), isEmpty);

      await repo.refresh('bob');
      expect(repo.messagesFor('bob').single.body, 'hi');
    });

    test(
      'a chat_received event appends to that peer\'s conversation',
      () async {
        final fake = FakePeerBeam();
        final repo = ChatRepository(api: fake);

        fake.emit(
          ChatReceived(
            ChatMessage(
              id: 'm2',
              peerId: 'alice',
              direction: 'in',
              body: 'yo',
              at: DateTime.now(),
              status: 'received',
            ),
          ),
        );
        await flush();

        expect(repo.messagesFor('alice').single.body, 'yo');
        // Untouched conversations stay empty.
        expect(repo.messagesFor('bob'), isEmpty);
      },
    );

    test('send shows the message immediately, then reconciles with the engine '
        '— keyed by the peer\'s real id, not its display name', () async {
      // Regression guard for the PeerTarget-carries-no-id bug: `id` and
      // `name` deliberately differ here. If `PeerTarget.id` weren't wired
      // through (or the fake/engine keyed by name again instead of id),
      // `chatSend` would persist under 'carol' while this test's `refresh`
      // reads 'pb-carol' — the reconcile step would find nothing and the
      // status would stay 'pending' forever, failing the last expectation.
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);
      const target = PeerTarget(
        id: 'pb-carol',
        name: 'carol',
        addresses: ['127.0.0.1'],
        port: 49600,
      );

      // Not awaited on purpose: the optimistic append happens synchronously
      // before chatSend's own await, exactly like the fire-and-forget call
      // the chat screen makes from a button handler.
      final pending = repo.send('pb-carol', target, '  hello  ');
      expect(repo.messagesFor('pb-carol').single.body, 'hello');
      expect(repo.messagesFor('pb-carol').single.status, 'pending');

      await pending;
      expect(fake.calls, contains('chatSend:hello'));
      // Reconciled from chatHistory('pb-carol'): the fake's persisted
      // record (keyed by peer.id) replaces the optimistic placeholder.
      expect(repo.messagesFor('pb-carol').single.status, 'sent');
      // The name is never used as a conversation key.
      expect(repo.messagesFor('carol'), isEmpty);
    });

    test('send ignores blank text', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);
      const target = PeerTarget(name: 'dave', addresses: [], port: 0);

      await repo.send('dave', target, '   ');

      expect(repo.messagesFor('dave'), isEmpty);
      expect(fake.calls, isEmpty);
    });

    test('a chat_status event flips the matching message\'s status in place, '
        'leaving other messages/fields untouched', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);

      // Seed the conversation via chat_received rather than refresh, so
      // this doesn't depend on the fake's chatHistory plumbing.
      final first = ChatMessage(
        id: 'm1',
        peerId: 'alice',
        direction: 'out',
        body: 'hi',
        at: DateTime.now(),
        status: 'pending',
      );
      final second = ChatMessage(
        id: 'm2',
        peerId: 'alice',
        direction: 'in',
        body: 'hey back',
        at: DateTime.now(),
        status: 'received',
      );
      fake.emit(ChatReceived(first));
      fake.emit(ChatReceived(second));
      await flush();

      fake.emit(
        const ChatStatus(messageId: 'm1', peerId: 'alice', status: 'sent'),
      );
      await flush();

      final messages = repo.messagesFor('alice');
      final updated = messages.firstWhere((m) => m.id == 'm1');
      expect(updated.status, 'sent');
      // Every other field on the updated message is untouched.
      expect(updated.peerId, first.peerId);
      expect(updated.direction, first.direction);
      expect(updated.body, first.body);
      expect(updated.at, first.at);
      // The other message in the same conversation is untouched.
      final other = messages.firstWhere((m) => m.id == 'm2');
      expect(other.status, 'received');
    });

    test('a chat_status event for an unknown peer or message id is a safe '
        'no-op', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);

      fake.emit(
        ChatReceived(
          ChatMessage(
            id: 'm1',
            peerId: 'alice',
            direction: 'out',
            body: 'hi',
            at: DateTime.now(),
            status: 'pending',
          ),
        ),
      );
      await flush();

      // Unknown peer id: no conversation exists yet for 'bob'.
      expect(
        () => fake.emit(
          const ChatStatus(messageId: 'm1', peerId: 'bob', status: 'sent'),
        ),
        returnsNormally,
      );
      await flush();
      expect(repo.messagesFor('bob'), isEmpty);

      // Unknown message id within a known conversation.
      expect(
        () => fake.emit(
          const ChatStatus(
            messageId: 'does-not-exist',
            peerId: 'alice',
            status: 'sent',
          ),
        ),
        returnsNormally,
      );
      await flush();
      expect(repo.messagesFor('alice').single.status, 'pending');
    });

    test('sendFile shows the file row immediately, then reconciles with the '
        'engine — keyed by the peer\'s real id, not its display name', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);
      const target = PeerTarget(
        id: 'pb-carol',
        name: 'carol',
        addresses: ['127.0.0.1'],
        port: 49600,
      );

      // Not awaited: the optimistic row is appended synchronously, before
      // chatSendFile's own await, exactly like the fire-and-forget call the
      // attach button makes.
      final pending = repo.sendFile(
        'pb-carol',
        target,
        '/tmp/report.pdf',
        name: 'report.pdf',
        size: 4096,
      );
      final optimistic = repo.messagesFor('pb-carol').single;
      expect(optimistic.isFile, isTrue);
      expect(optimistic.fileName, 'report.pdf');
      expect(optimistic.fileSize, 4096);
      expect(optimistic.status, ChatStatusValue.transferring);
      expect(optimistic.isMine, isTrue);
      // A file row carries no text: rendering `body` would be a blank bubble.
      expect(optimistic.body, isEmpty);

      await pending;
      expect(fake.calls, contains('chatSendFile:/tmp/report.pdf'));
      // Reconciled from chatHistory('pb-carol'): the persisted record (keyed
      // by peer.id) replaces the optimistic placeholder.
      final settled = repo.messagesFor('pb-carol').single;
      expect(settled.id, 'file-1');
      expect(settled.isFile, isTrue);
      // The name is never used as a conversation key.
      expect(repo.messagesFor('carol'), isEmpty);
    });

    test('sendFile is called once per file when several are attached', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);
      const target = PeerTarget(
        id: 'pb-bob',
        name: 'bob',
        addresses: ['127.0.0.1'],
        port: 49600,
      );

      await Future.wait([
        repo.sendFile('pb-bob', target, '/tmp/a.bin', name: 'a.bin'),
        repo.sendFile('pb-bob', target, '/tmp/b.bin', name: 'b.bin'),
        repo.sendFile('pb-bob', target, '/tmp/c.bin', name: 'c.bin'),
      ]);

      expect(
        fake.calls.where((c) => c.startsWith('chatSendFile:')),
        ['chatSendFile:/tmp/a.bin', 'chatSendFile:/tmp/b.bin', 'chatSendFile:/tmp/c.bin'],
      );
      expect(repo.messagesFor('pb-bob'), hasLength(3));
    });

    test('a local sendFile failure marks the row failed and keeps the reason '
        '(nothing was persisted, so it must not just vanish)', () async {
      final fake = FakePeerBeam()..failChatSendFile = true;
      final repo = ChatRepository(api: fake);
      const target = PeerTarget(
        id: 'pb-bob',
        name: 'bob',
        addresses: ['127.0.0.1'],
        port: 49600,
      );

      await repo.sendFile('pb-bob', target, '/tmp/gone.bin', name: 'gone.bin');

      final row = repo.messagesFor('pb-bob').single;
      expect(row.status, ChatStatusValue.failed);
      expect(repo.errorFor(row.id), contains('cannot read'));
      expect(repo.isUnsent('pb-bob', row.id), isTrue);

      // The engine persisted NOTHING for this row, so every later reconcile
      // reads a history that does not contain it. It must survive them all —
      // any sibling send, any incoming message, or reopening the thread would
      // otherwise erase the only evidence the file never left.
      await repo.refresh('pb-bob');
      await repo.refresh('pb-bob');
      final after = repo.messagesFor('pb-bob').single;
      expect(after.id, row.id);
      expect(after.status, ChatStatusValue.failed);
      expect(repo.errorFor(row.id), contains('cannot read'));

      // Until the user acknowledges it.
      repo.dismiss('pb-bob', row.id);
      expect(repo.messagesFor('pb-bob'), isEmpty);
      expect(repo.errorFor(row.id), isNull);
      await repo.refresh('pb-bob');
      expect(repo.messagesFor('pb-bob'), isEmpty);
    });

    test('a mixed multi-select keeps the refused file visible alongside the '
        'ones that went through', () async {
      final fake = FakePeerBeam()..refusedFilePaths.add('/tmp/b.bin');
      final repo = ChatRepository(api: fake);
      const target = PeerTarget(
        id: 'pb-bob',
        name: 'bob',
        addresses: ['127.0.0.1'],
        port: 49600,
      );

      await Future.wait([
        repo.sendFile('pb-bob', target, '/tmp/a.bin', name: 'a.bin'),
        repo.sendFile('pb-bob', target, '/tmp/b.bin', name: 'b.bin'),
        repo.sendFile('pb-bob', target, '/tmp/c.bin', name: 'c.bin'),
      ]);

      final rows = repo.messagesFor('pb-bob');
      expect(rows, hasLength(3));
      expect(rows.map((m) => m.fileName), containsAll(['a.bin', 'b.bin']));
      final refused = rows.singleWhere((m) => m.fileName == 'b.bin');
      expect(refused.status, ChatStatusValue.failed);
      expect(repo.errorFor(refused.id), contains('cannot read'));
      // The two that were accepted are the engine's own persisted rows.
      expect(
        rows.where((m) => m.status == ChatStatusValue.transferring),
        hasLength(2),
      );
    });

    test('a chat_status for a file row flips its status and keeps the error '
        'the engine sent with it', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);

      fake.emit(
        ChatReceived(
          ChatMessage(
            id: 'fr-1',
            peerId: 'pb-bob',
            direction: 'out',
            body: '',
            at: DateTime.now(),
            status: ChatStatusValue.transferring,
            kind: ChatMessageKind.file,
            fileName: 'a.bin',
            fileSize: 7,
          ),
        ),
      );
      await flush();

      fake.emit(
        const ChatStatus(
          messageId: 'fr-1',
          peerId: 'pb-bob',
          status: ChatStatusValue.failed,
          error: 'cannot reach Bob to send a.bin: no route',
        ),
      );
      await flush();

      final row = repo.messagesFor('pb-bob').single;
      expect(row.status, ChatStatusValue.failed);
      // Every other field survives the flip.
      expect(row.isFile, isTrue);
      expect(row.fileName, 'a.bin');
      expect(row.fileSize, 7);
      expect(repo.errorFor('fr-1'), 'cannot reach Bob to send a.bin: no route');
    });

    test('a received file re-reads the conversation so the row learns where '
        'the file actually landed', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);

      // The offer arrives on the CHAT channel first: no local path yet.
      fake.emit(
        ChatReceived(
          ChatMessage(
            id: 'fr-1',
            peerId: 'pb-bob',
            direction: 'in',
            body: '',
            at: DateTime.now(),
            status: ChatStatusValue.pendingApproval,
            kind: ChatMessageKind.file,
            fileName: 'a.bin',
            fileSize: 7,
          ),
        ),
      );
      await flush();
      expect(repo.messagesFor('pb-bob').single.localPath, isNull);

      // The engine writes `file.local_path` on the persisted record BEFORE it
      // settles the row, so a re-read after `received` picks the path up. The
      // status event itself never carries it.
      fake.chatHistories['pb-bob'] = [
        ChatMessage(
          id: 'fr-1',
          peerId: 'pb-bob',
          direction: 'in',
          body: '',
          at: DateTime.now(),
          status: ChatStatusValue.received,
          kind: ChatMessageKind.file,
          fileName: 'a.bin',
          fileSize: 7,
          localPath: '/home/me/Downloads/a.bin',
        ),
      ];
      fake.emit(
        const ChatStatus(
          messageId: 'fr-1',
          peerId: 'pb-bob',
          status: ChatStatusValue.received,
        ),
      );
      await flush();
      await flush();

      final row = repo.messagesFor('pb-bob').single;
      expect(row.status, ChatStatusValue.received);
      expect(row.localPath, '/home/me/Downloads/a.bin');
    });

    test('a text chat_status never triggers a re-read', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);

      fake.emit(
        ChatReceived(
          ChatMessage(
            id: 'm1',
            peerId: 'pb-bob',
            direction: 'out',
            body: 'hi',
            at: DateTime.now(),
            status: ChatStatusValue.pending,
          ),
        ),
      );
      await flush();

      fake.emit(
        const ChatStatus(
          messageId: 'm1',
          peerId: 'pb-bob',
          status: ChatStatusValue.sent,
        ),
      );
      await flush();
      await flush();

      expect(fake.calls.where((c) => c.startsWith('chatHistory:')), isEmpty);
      expect(repo.messagesFor('pb-bob').single.status, ChatStatusValue.sent);
    });

    test('messagesFor keeps text and file rows distinct in one thread', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);

      fake.emit(
        ChatReceived(
          ChatMessage(
            id: 'm1',
            peerId: 'pb-bob',
            direction: 'in',
            body: 'here you go',
            at: DateTime.now(),
            status: ChatStatusValue.received,
          ),
        ),
      );
      fake.emit(
        ChatReceived(
          ChatMessage(
            id: 'fr-1',
            peerId: 'pb-bob',
            direction: 'in',
            body: '',
            at: DateTime.now(),
            status: ChatStatusValue.pendingApproval,
            kind: ChatMessageKind.file,
            fileName: 'a.bin',
            fileSize: 7,
          ),
        ),
      );
      await flush();

      final rows = repo.messagesFor('pb-bob');
      expect(rows, hasLength(2));
      expect(rows.first.isFile, isFalse);
      expect(rows.first.body, 'here you go');
      expect(rows.last.isFile, isTrue);
      expect(rows.last.fileName, 'a.bin');
    });
  });
}
