// Repository tests over a mock SDK — no native library. Prove repositories are
// event-driven (state updates from engine events) and delegate commands to the
// SDK (no transfer logic in Dart).

import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/data/chat_repository.dart';
import 'package:peerbeam/data/discovery_repository.dart';
import 'package:peerbeam/data/history_repository.dart';
import 'package:peerbeam/data/transfer_repository.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/exceptions.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/models.dart' as ui;

import '../sdk/fake_peerbeam.dart';

/// Flush pending microtasks so stream listeners run.
Future<void> flush() => Future(() {});

/// An engine whose reconcile call fails — opening a thread must still show it.
class _ReconcileFailsPeerBeam extends FakePeerBeam {
  @override
  Future<int> chatReconcile(String peerId) async =>
      throw const InternalException('reconcile exploded');
}

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

    // ── interrupted transfers ────────────────────────────────
    //
    // The state that outlives the process. Everything here is about a row that
    // no event of this session created and no event will ever complete.

    InterruptedTransfer cp(
      String id, {
      String direction = 'sending',
      int done = 400,
      int total = 1000,
      bool resumable = true,
    }) => InterruptedTransfer(
      id: id,
      direction: direction,
      peerId: 'pb-peer-1',
      file: '$id.bin',
      path: '/save/$id.bin',
      transferredBytes: done,
      totalBytes: total,
      startedAt: '',
      resumable: resumable,
    );

    test('a surviving checkpoint becomes an interrupted row with the progress '
        'it actually reached', () async {
      final fake = FakePeerBeam();
      fake.interrupted = [cp('t-old', done: 400, total: 1000)];
      final repo = TransferRepository(api: fake);

      await repo.refreshInterrupted();
      final t = repo.transfers.single;
      expect(t.id, 't-old');
      expect(t.state, ui.TransferState.interrupted);
      // Not a fresh zero-progress row: how far it got is the whole reason to
      // resume rather than restart.
      expect(t.doneBytes, 400);
      expect(t.totalBytes, 1000);
      expect(t.progress, 0.4);
      expect(t.resumable, isTrue);
      // And it is not counted as work in progress — nothing is moving.
      expect(repo.activeCount, 0);
      expect(repo.awaitingApproval, isEmpty);
    });

    test('an inbound checkpoint is not resumable from this side', () async {
      final fake = FakePeerBeam();
      fake.interrupted = [cp('t-in', direction: 'receiving', resumable: false)];
      final repo = TransferRepository(api: fake);

      await repo.refreshInterrupted();
      final t = repo.transfers.single;
      expect(t.direction, ui.TransferDirection.receiving);
      expect(
        t.resumable,
        isFalse,
        reason:
            'the transfer protocol is sender-driven — a Resume here would do '
            'nothing',
      );
    });

    test('a live transfer wins over a checkpoint bearing its id', () async {
      final fake = FakePeerBeam();
      fake.interrupted = [cp('t1')];
      final repo = TransferRepository(api: fake);

      fake.emit(ev('transfer_queued', 't1', {'peer': 'Bob', 'file': 'a.bin'}));
      await flush();
      await repo.refreshInterrupted();

      expect(repo.transfers.single.state, ui.TransferState.pending);
      expect(repo.transfers.single.peerName, 'Bob');
    });

    test('transfer_interrupted brings a row back after its terminal event '
        'removed it', () async {
      final fake = FakePeerBeam();
      final repo = TransferRepository(api: fake);

      fake.emit(ev('transfer_queued', 't1', {'peer': 'Bob', 'file': 'a.bin'}));
      fake.emit(ev('transfer_failed', 't1', {'error': {'code': 'connection'}}));
      await flush();
      expect(repo.transfers, isEmpty);

      fake.emit(
        ev('transfer_interrupted', 't1', {
          'peer_id': 'pb-peer-1',
          'file': 'a.bin',
          'direction': 'sending',
          'resumable': true,
          'stats': {'transferred_bytes': 700, 'total_bytes': 1000},
        }),
      );
      await flush();
      final t = repo.transfers.single;
      expect(t.state, ui.TransferState.interrupted);
      expect(t.doneBytes, 700);
      expect(t.resumable, isTrue);
    });

    test('resumeInterrupted goes to the engine, and is not resume', () async {
      final fake = FakePeerBeam();
      fake.interrupted = [cp('t-old')];
      final repo = TransferRepository(api: fake);
      await repo.refreshInterrupted();

      repo.resumeInterrupted('t-old');
      await flush();
      expect(fake.calls, contains('resumeInterrupted:t-old'));
      expect(
        fake.calls,
        isNot(contains('resume:t-old')),
        reason:
            'resume un-pauses a live transfer; these are two different verbs '
            'and calling the wrong one would silently do nothing',
      );
    });

    test('a refused resume is surfaced, never swallowed', () async {
      final fake = FakePeerBeam();
      fake.interrupted = [cp('t-old')];
      fake.unresumableIds = {'t-old'};
      final repo = TransferRepository(api: fake);
      await repo.refreshInterrupted();

      final errors = <String>[];
      final sub = repo.errors.listen(errors.add);
      repo.resumeInterrupted('t-old');
      await flush();
      await flush();
      await sub.cancel();

      expect(errors, isNotEmpty);
      expect(errors.single, contains('resume'));
      // And the row stays: a refusal changed nothing.
      expect(repo.transfers.single.state, ui.TransferState.interrupted);
    });

    test('discarding removes the row', () async {
      final fake = FakePeerBeam();
      fake.interrupted = [cp('t-old')];
      final repo = TransferRepository(api: fake);
      await repo.refreshInterrupted();

      repo.discardInterrupted('t-old');
      await flush();
      expect(fake.calls, contains('discardInterrupted:t-old'));

      fake.emit(ev('transfer_discarded', 't-old'));
      await flush();
      expect(repo.transfers, isEmpty);
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
      repo.accept('t1');
      repo.acceptTrust('t1');
      repo.reject('t1');
      await flush();
      expect(
        fake.calls,
        containsAll([
          'pause:t1',
          'resume:t1',
          'cancel:t1',
          'accept:t1',
          'acceptTrust:t1',
          'reject:t1',
        ]),
      );
    });

    test(
      'an approval the engine refuses is surfaced, never swallowed',
      () async {
        // The approval actions are the user's consent, and they only mean
        // anything while a decision is genuinely open. They used to
        // `.catchError((_) {})`, so a tap on a stale Accept/Decline did
        // nothing at all and looked exactly like success — the same shape as
        // this project's pairing-gate fail-open.
        final fake = FakePeerBeam()..noPendingDecisionIds.add('t7');
        final repo = TransferRepository(api: fake);
        final errors = <String>[];
        repo.errors.listen(errors.add);

        fake.emit(
          ev('transfer_queued', 't7', {
            'peer': 'Bob',
            'file': 'report.pdf',
            'incoming': true,
          }),
        );
        await flush();

        repo.accept('t7');
        await flush();
        expect(errors, hasLength(1));
        expect(errors.single, contains('report.pdf'));
        expect(errors.single, contains("Couldn't accept"));

        repo.reject('t7');
        await flush();
        expect(errors, hasLength(2));
        expect(errors.last, contains("Couldn't decline"));

        // …and a refusal must not throw out of the fire-and-forget call site.
        expect(fake.calls, containsAll(['accept:t7', 'reject:t7']));
      },
    );

    test('awaitingApproval holds only inbound transfers still awaiting a '
        'decision', () async {
      final fake = FakePeerBeam();
      final repo = TransferRepository(api: fake);

      fake.emit(
        ev('transfer_queued', 'in-1', {'file': 'a.bin', 'incoming': true}),
      );
      // Our own send: never anyone's to approve.
      fake.emit(
        ev('transfer_queued', 'out-1', {'file': 'b.bin', 'incoming': false}),
      );
      // Inbound but already approved and running.
      fake.emit(
        ev('transfer_queued', 'in-live', {'file': 'c.bin', 'incoming': true}),
      );
      fake.emit(ev('transfer_started', 'in-live'));
      fake.emit(
        ev('transfer_queued', 'in-2', {'file': 'd.bin', 'incoming': true}),
      );
      await flush();

      expect(repo.transfers, hasLength(4));
      expect(repo.awaitingApproval.map((t) => t.id), ['in-1', 'in-2']);
    });

    // The security-relevant one, at the repository seam. `acceptTrust` grants
    // a device persistent auto-accept for everything it sends from then on;
    // a bulk action answers only the batch on screen and must never widen
    // into that.
    test('acceptAll accepts each waiting inbound transfer exactly once and '
        'never trusts', () async {
      final fake = FakePeerBeam();
      final repo = TransferRepository(api: fake);

      fake.emit(
        ev('transfer_queued', 'in-1', {'file': 'a.bin', 'incoming': true}),
      );
      fake.emit(
        ev('transfer_queued', 'out-1', {'file': 'b.bin', 'incoming': false}),
      );
      fake.emit(
        ev('transfer_queued', 'in-2', {'file': 'c.bin', 'incoming': true}),
      );
      await flush();

      final result = await repo.acceptAll();

      expect(fake.calls, ['accept:in-1', 'accept:in-2']);
      expect(fake.calls.where((c) => c.startsWith('acceptTrust:')), isEmpty);
      expect(result, (requested: 2, settled: 2, gone: 0, failed: 0));
    });

    // Aggregate, don't shout: five accepts where two had already settled used
    // to mean two separate error snackbars, which reads as breakage rather
    // than as the ordinary race it is.
    test('a bulk decision counts what the engine actually answered, and keeps '
        'the per-failure errors off the error stream', () async {
      final fake = FakePeerBeam()..noPendingDecisionIds.addAll(['in-2', 'in-4']);
      final repo = TransferRepository(api: fake);
      final errors = <String>[];
      repo.errors.listen(errors.add);

      for (final id in ['in-1', 'in-2', 'in-3', 'in-4']) {
        fake.emit(
          ev('transfer_queued', id, {'file': '$id.bin', 'incoming': true}),
        );
      }
      await flush();

      final result = await repo.declineAll();
      await flush();

      expect(fake.calls, [
        'reject:in-1',
        'reject:in-2',
        'reject:in-3',
        'reject:in-4',
      ]);
      expect(result, (requested: 4, settled: 2, gone: 2, failed: 0));
      expect(errors, isEmpty);
    });

    test('a bulk decision with nothing waiting asks the engine nothing', () async {
      final fake = FakePeerBeam();
      final repo = TransferRepository(api: fake);

      expect(await repo.acceptAll(), (
        requested: 0,
        settled: 0,
        gone: 0,
        failed: 0,
      ));
      expect(fake.calls, isEmpty);
    });

    // `settled + gone + failed == requested` is what `_bulkReport` reads to
    // decide what to tell the user, so it has to hold on every return —
    // including the one taken when there is no engine at all. Counting those
    // ids as neither settled nor gone nor failed made the report say "None
    // were still waiting" about a batch on which no attempt was ever made.
    // `gone` would be just as wrong: nothing here knows anything stopped
    // waiting. Latent while `AppState.live` always supplies an api; the
    // invariant is not conditional on that staying true.
    test('a bulk decision with no engine counts every id as failed, keeping '
        'the outcome invariant', () async {
      final repo = TransferRepository(api: null);

      expect(await repo.acceptOnly(['in-1', 'in-2', 'in-3']), (
        requested: 3,
        settled: 0,
        gone: 0,
        failed: 3,
      ));
      expect(await repo.declineOnly(['in-1']), (
        requested: 1,
        settled: 0,
        gone: 0,
        failed: 1,
      ));
      // And an empty batch is still nothing at all, not a failure.
      expect(await repo.acceptOnly(const []), (
        requested: 0,
        settled: 0,
        gone: 0,
        failed: 0,
      ));
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

    test('openThread settles crash-orphaned rows BEFORE it reads the thread, '
        'so nothing renders as in-flight that never will be', () async {
      final fake = FakePeerBeam();
      fake.chatHistories['pb-bob'] = [
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
      ];
      final repo = ChatRepository(api: fake);

      await repo.openThread('pb-bob');

      // Order matters: reading first would render a dead Accept button.
      expect(fake.calls, ['chatReconcile:pb-bob', 'chatHistory:pb-bob']);
      expect(
        repo.messagesFor('pb-bob').single.status,
        ChatStatusValue.interrupted,
      );
    });

    test('openThread still loads the conversation when the reconcile fails', () async {
      final fake = _ReconcileFailsPeerBeam();
      fake.chatHistories['pb-bob'] = [
        ChatMessage(
          id: 'm1',
          peerId: 'pb-bob',
          direction: 'in',
          body: 'hi',
          at: DateTime.now(),
          status: ChatStatusValue.received,
        ),
      ];
      final repo = ChatRepository(api: fake);

      await repo.openThread('pb-bob');

      expect(repo.messagesFor('pb-bob').single.body, 'hi');
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
      // `staging`, not `transferring`: the engine has not copied a byte yet,
      // and claiming bytes are moving would render "Sending…" over a file it
      // has not read — and hide the Cancel this row is entitled to.
      expect(optimistic.status, ChatStatusValue.staging);
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
      // The two that were accepted are the engine's own persisted rows —
      // `staging`, because that is the state a share is persisted in before a
      // byte of it has been copied.
      expect(
        rows.where((m) => m.status == ChatStatusValue.staging),
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

    // ── 2b: staging, cancel, conversations ──────────────────────

    test('staging progress is kept per message and retired when the row '
        'leaves staging', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);

      // Nothing known yet: an indeterminate bar, never a fabricated 0%.
      expect(repo.stagingFor('fr-1'), isNull);

      fake.emit(
        const ChatStatus(
          messageId: 'fr-1',
          peerId: 'pb-bob',
          status: ChatStatusValue.staging,
          progress: (done: 2048, total: 8192),
        ),
      );
      await flush();
      expect(repo.stagingFor('fr-1')?.done, 2048);
      expect(repo.stagingFor('fr-1')?.total, 8192);

      // Later ticks replace it.
      fake.emit(
        const ChatStatus(
          messageId: 'fr-1',
          peerId: 'pb-bob',
          status: ChatStatusValue.staging,
          progress: (done: 6144, total: 8192),
        ),
      );
      await flush();
      expect(repo.stagingFor('fr-1')?.done, 6144);

      // The copy finished and the entry was queued: the bar has nothing left
      // to say, so it must not linger for the next row to inherit.
      fake.emit(
        const ChatStatus(
          messageId: 'fr-1',
          peerId: 'pb-bob',
          status: ChatStatusValue.pending,
        ),
      );
      await flush();
      expect(repo.stagingFor('fr-1'), isNull);
    });

    // The engine emits one bare `staging` event before the first byte moves.
    // Arriving after real progress it must NOT wipe the bar back to
    // indeterminate — only a status that is not staging retires it.
    test('a bare staging event never erases progress already known', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);

      fake.emit(
        const ChatStatus(
          messageId: 'fr-1',
          peerId: 'pb-bob',
          status: ChatStatusValue.staging,
          progress: (done: 2048, total: 8192),
        ),
      );
      await flush();
      fake.emit(
        const ChatStatus(
          messageId: 'fr-1',
          peerId: 'pb-bob',
          status: ChatStatusValue.staging,
        ),
      );
      await flush();

      expect(repo.stagingFor('fr-1')?.done, 2048);
    });

    // Progress arrives for the engine's own message id while the conversation
    // still holds the optimistic row under a local one, so it cannot be
    // conditional on finding a row.
    test('progress is kept even for a row this session has not read yet', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);

      fake.emit(
        const ChatStatus(
          messageId: 'file-1',
          peerId: 'pb-nobody',
          status: ChatStatusValue.staging,
          progress: (done: 1, total: 4),
        ),
      );
      await flush();

      expect(repo.messagesFor('pb-nobody'), isEmpty);
      expect(repo.stagingFor('file-1')?.done, 1);
    });

    test('cancelFile stops a queued share and the engine settles the row', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);
      fake.chatHistories['pb-bob'] = [
        ChatMessage(
          id: 'fr-1',
          peerId: 'pb-bob',
          direction: 'out',
          body: '',
          at: DateTime.now(),
          status: ChatStatusValue.pending,
          kind: ChatMessageKind.file,
          fileName: 'movie.mkv',
          fileSize: 7,
        ),
      ];
      await repo.refresh('pb-bob');

      expect(await repo.cancelFile('pb-bob', 'fr-1'), isTrue);
      await flush();

      expect(fake.calls, contains('chatCancel:pb-bob/fr-1'));
      final row = repo.messagesFor('pb-bob').single;
      expect(row.status, ChatStatusValue.failed);
      expect(repo.errorFor('fr-1'), 'cancelled');
    });

    // The honest false. The engine cancelled nothing because the file had
    // already gone — so the row must NOT be removed or relabelled as though
    // the user had stopped it, and the conversation is re-read instead.
    test('a cancel the engine refuses leaves the row exactly as it is', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);
      fake.chatHistories['pb-bob'] = [
        ChatMessage(
          id: 'fr-1',
          peerId: 'pb-bob',
          direction: 'out',
          body: '',
          at: DateTime.now(),
          // Already delivered: `is_cancellable_outgoing_file` refuses it.
          status: ChatStatusValue.sent,
          kind: ChatMessageKind.file,
          fileName: 'movie.mkv',
          fileSize: 7,
        ),
      ];
      await repo.refresh('pb-bob');

      expect(await repo.cancelFile('pb-bob', 'fr-1'), isFalse);
      await flush();

      final row = repo.messagesFor('pb-bob').single;
      expect(row.id, 'fr-1', reason: 'the row is not removed');
      expect(
        row.status,
        ChatStatusValue.sent,
        reason: 'and it still reads sent',
      );
      // Re-read, so what is on screen is what the engine actually holds.
      expect(
        fake.calls.where((c) => c == 'chatHistory:pb-bob'),
        hasLength(2),
      );
    });

    test('an inbound offer is never cancellable — that is the approval gate\'s '
        'business', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);
      fake.chatHistories['pb-bob'] = [
        ChatMessage(
          id: 'fr-1',
          peerId: 'pb-bob',
          direction: 'in',
          body: '',
          at: DateTime.now(),
          status: ChatStatusValue.pendingApproval,
          kind: ChatMessageKind.file,
          fileName: 'theirs.bin',
          fileSize: 7,
        ),
      ];
      await repo.refresh('pb-bob');

      expect(await repo.cancelFile('pb-bob', 'fr-1'), isFalse);
      expect(
        repo.messagesFor('pb-bob').single.status,
        ChatStatusValue.pendingApproval,
      );
    });

    test('refreshConversations lists every thread, newest first, keyed by the '
        'peer id', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);
      final old = DateTime.parse('2026-08-10T09:00:00Z');
      final recent = DateTime.parse('2026-08-13T09:00:00Z');
      // A thread whose only row is a queued file for a peer nothing on the
      // network can currently see — the case this list exists for.
      fake.chatHistories['pb-quiet'] = [
        ChatMessage(
          id: 'fr-1',
          peerId: 'pb-quiet',
          direction: 'out',
          body: '',
          at: old,
          status: ChatStatusValue.pending,
          kind: ChatMessageKind.file,
          fileName: 'movie.mkv',
          fileSize: 7,
        ),
      ];
      fake.chatHistories['pb-busy'] = [
        ChatMessage(
          id: 'fr-2',
          peerId: 'pb-busy',
          direction: 'in',
          body: '',
          at: recent,
          status: ChatStatusValue.pendingApproval,
          kind: ChatMessageKind.file,
          fileName: 'theirs.bin',
          fileSize: 7,
        ),
      ];

      expect(repo.conversations, isEmpty);
      await repo.refreshConversations();

      final list = repo.conversations;
      expect(list.map((c) => c.peerId), ['pb-busy', 'pb-quiet']);
      // Only rows genuinely awaiting a decision count — not our own outgoing
      // file, and never text.
      expect(list.first.unreadHint, 1);
      expect(list.first.needsAttention, isTrue);
      expect(list.last.unreadHint, 0);
      expect(list.last.needsAttention, isFalse);
      expect(list.last.lastAt, old);
    });

    test('an arriving message refreshes the conversation list, and a staging '
        'tick does not', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);
      // The engine persists the record before it announces it, so the thread
      // is already on disk when the event lands.
      fake.chatHistories['pb-new'] = [
        ChatMessage(
          id: 'm1',
          peerId: 'pb-new',
          direction: 'in',
          body: 'hi',
          at: DateTime.now(),
          status: ChatStatusValue.received,
        ),
      ];

      fake.emit(
        ChatReceived(
          ChatMessage(
            id: 'm1',
            peerId: 'pb-new',
            direction: 'in',
            body: 'hi',
            at: DateTime.now(),
            status: ChatStatusValue.received,
          ),
        ),
      );
      await flush();
      await flush();
      expect(fake.calls.where((c) => c == 'chatConversations'), hasLength(1));
      // A thread that did not exist a moment ago is now reachable.
      expect(repo.conversations.single.peerId, 'pb-new');

      // ~100 of these arrive per share. Each one costs a full scan of every
      // conversation on disk, and none of them can change the list.
      for (var i = 0; i < 5; i++) {
        fake.emit(
          ChatStatus(
            messageId: 'fr-1',
            peerId: 'pb-new',
            status: ChatStatusValue.staging,
            progress: (done: i, total: 5),
          ),
        );
      }
      await flush();
      await flush();
      expect(fake.calls.where((c) => c == 'chatConversations'), hasLength(1));

      // A settled status can change what is waiting on the user, so it does.
      fake.emit(
        const ChatStatus(
          messageId: 'fr-1',
          peerId: 'pb-new',
          status: ChatStatusValue.sent,
        ),
      );
      await flush();
      await flush();
      expect(fake.calls.where((c) => c == 'chatConversations'), hasLength(2));
    });

    // The first message to a peer creates the conversation. An offline peer
    // sends back no record and settles no status, so nothing else would ever
    // announce the thread — and it would be missing from the Conversations
    // list for exactly as long as the peer is unreachable.
    test('starting a conversation puts it on the list without waiting for the '
        'peer', () async {
      final fake = FakePeerBeam();
      final repo = ChatRepository(api: fake);
      const target = PeerTarget(
        id: 'pb-offline',
        name: 'offline',
        addresses: ['10.0.0.9'],
        port: 49600,
      );

      await repo.send('pb-offline', target, 'are you there');
      await flush();

      expect(repo.conversations.single.peerId, 'pb-offline');
      expect(repo.conversations.single.needsAttention, isFalse);

      await repo.sendFile('pb-offline', target, '/tmp/a.bin', name: 'a.bin');
      await flush();

      expect(repo.conversations.single.peerId, 'pb-offline');
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
