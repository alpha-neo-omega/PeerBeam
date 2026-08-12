// Chat screen behaviours for file-in-chat (increment 2a, online only):
// the attach button's fan-out over a multi-select, the file bubble rendered
// from the PERSISTED record, inline approval on an incoming offer, and the
// Android SAF fallback when a received file's recorded path no longer exists.

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/chat/chat_screen.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

const _peer = PeerTarget(
  id: 'pb-bob',
  name: 'Bob',
  addresses: ['127.0.0.1'],
  port: 49600,
);

Widget _screen(AppState state) => AppScope(
  state: state,
  child: const MaterialApp(
    home: ChatScreen(peerId: 'pb-bob', peer: _peer),
  ),
);

/// Pump the screen and let its post-frame `refresh` land.
Future<AppState> _open(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(_screen(state));
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 300));
  return state;
}

ChatMessage _file({
  required String id,
  required String direction,
  required String status,
  String name = 'report.pdf',
  int size = 4096,
  String? localPath,
}) => ChatMessage(
  id: id,
  peerId: 'pb-bob',
  direction: direction,
  body: '',
  at: DateTime.now(),
  status: status,
  kind: ChatMessageKind.file,
  fileName: name,
  fileSize: size,
  localPath: localPath,
);

void main() {
  testWidgets('the attach button sends EVERY picked file, not just the first', (
    tester,
  ) async {
    // flutter_test's target platform is Android, so `pickFilesToStage` takes
    // the native `peerbeam/android` branch; stub a three-file multi-select.
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      const MethodChannel('peerbeam/android'),
      (call) async => call.method == 'pickFiles'
          ? [
              {'path': '/tmp/a.bin', 'name': 'a.bin', 'size': 1},
              {'path': '/tmp/b.bin', 'name': 'b.bin', 'size': 2},
              {'path': '/tmp/c.bin', 'name': 'c.bin', 'size': 3},
            ]
          : null,
    );
    final fake = FakePeerBeam();
    await _open(tester, fake);

    await tester.tap(find.byIcon(Icons.attach_file_rounded));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(fake.calls.where((c) => c.startsWith('chatSendFile:')), [
      'chatSendFile:/tmp/a.bin',
      'chatSendFile:/tmp/b.bin',
      'chatSendFile:/tmp/c.bin',
    ]);
    // All three rows are in the thread.
    expect(find.text('a.bin'), findsOneWidget);
    expect(find.text('b.bin'), findsOneWidget);
    expect(find.text('c.bin'), findsOneWidget);
  });

  testWidgets('a file the engine refuses stays visible next to the ones that '
      'went through, and clears only when dismissed', (tester) async {
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      const MethodChannel('peerbeam/android'),
      (call) async => call.method == 'pickFiles'
          ? [
              {'path': '/tmp/a.bin', 'name': 'a.bin', 'size': 1},
              {'path': '/tmp/gone.bin', 'name': 'gone.bin', 'size': 2},
            ]
          : null,
    );
    final fake = FakePeerBeam()..refusedFilePaths.add('/tmp/gone.bin');
    await _open(tester, fake);

    await tester.tap(find.byIcon(Icons.attach_file_rounded));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    // The accepted sibling's own reconcile has already run by now; the
    // refused row must have survived it.
    expect(find.text('a.bin'), findsOneWidget);
    expect(find.text('gone.bin'), findsOneWidget);
    expect(find.textContaining('cannot read'), findsOneWidget);

    // Let the staggered entrance animation settle before tapping (the row's
    // indeterminate progress bar animates forever, so pumpAndSettle can't be
    // used here).
    await tester.pump(const Duration(seconds: 1));
    await tester.tap(find.text('Dismiss'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.text('gone.bin'), findsNothing);
    expect(find.text('a.bin'), findsOneWidget);
  });

  testWidgets('a failed outgoing row never shows the delivered tick', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      _file(id: 'fr-1', direction: 'out', status: ChatStatusValue.failed),
      _file(id: 'fr-2', direction: 'out', status: ChatStatusValue.sent),
    ];
    await _open(tester, fake);

    // Exactly one tick: the row that really was delivered.
    expect(find.byIcon(Icons.check_rounded), findsOneWidget);
    expect(find.byIcon(Icons.error_outline_rounded), findsWidgets);
  });

  testWidgets('an in-flight outgoing row shows a pending marker, not a tick', (
    tester,
  ) async {
    final fake = FakePeerBeam()..liveTransferIds.add('fr-1');
    fake.chatHistories['pb-bob'] = [
      _file(
        id: 'fr-1',
        direction: 'out',
        status: ChatStatusValue.transferring,
      ),
    ];
    await _open(tester, fake);

    expect(find.byIcon(Icons.check_rounded), findsNothing);
    expect(find.byIcon(Icons.schedule), findsOneWidget);
  });

  testWidgets('a file row renders its name and size, never a blank bubble', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      _file(id: 'fr-1', direction: 'out', status: ChatStatusValue.sent),
    ];
    await _open(tester, fake);

    expect(find.text('report.pdf'), findsOneWidget);
    expect(find.textContaining('4.0 KB'), findsOneWidget);
    expect(find.textContaining('Sent'), findsWidgets);
  });

  testWidgets('a legacy record (no kind/file keys) still renders as text', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    // Decoded from the raw JSON an engine predating file-in-chat persisted.
    fake.chatHistories['pb-bob'] = [
      ChatMessage.fromJson(const {
        'id': 'm-legacy',
        'peer_id': 'pb-bob',
        'direction': 'in',
        'timestamp': '2026-08-10T12:00:00Z',
        'body': 'hello from 1a',
        'status': 'received',
      }),
    ];
    await _open(tester, fake);

    expect(find.text('hello from 1a'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('an incoming offer shows inline Accept / Trust / Decline wired '
      'to the existing transfer approval', (tester) async {
    final fake = FakePeerBeam();
    // Its transfer is registered and waiting on the decision — this is a live
    // offer, not one a crash left behind.
    fake.liveTransferIds.add('fr-1');
    fake.chatHistories['pb-bob'] = [
      _file(
        id: 'fr-1',
        direction: 'in',
        status: ChatStatusValue.pendingApproval,
      ),
    ];
    await _open(tester, fake);

    expect(find.text('Accept'), findsOneWidget);
    expect(find.text('Trust'), findsOneWidget);
    expect(find.text('Decline'), findsOneWidget);

    // The ids match by construction: the chat message id IS the transfer id.
    await tester.tap(find.text('Accept'));
    await tester.pump();
    expect(fake.calls, contains('accept:fr-1'));

    await tester.tap(find.text('Trust'));
    await tester.pump();
    expect(fake.calls, contains('acceptTrust:fr-1'));

    await tester.tap(find.text('Decline'));
    await tester.pump();
    expect(fake.calls, contains('reject:fr-1'));
  });

  // A row left `pendingapproval` by a crash has no transfer behind it any
  // more, so its Accept button would be dead. Opening the thread reconciles
  // first, and the row renders as interrupted instead.
  testWidgets('a crash-orphaned offer is settled on open, not left offering a '
      'dead Accept', (tester) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      _file(
        id: 'fr-1',
        direction: 'in',
        status: ChatStatusValue.pendingApproval,
      ),
    ];
    await _open(tester, fake);

    expect(fake.calls, contains('chatReconcile:pb-bob'));
    expect(find.text('Accept'), findsNothing);
    expect(find.textContaining('Interrupted'), findsOneWidget);
  });

  // Auto-accept + a trusted device: the engine short-circuits its approval
  // wait entirely, so it never opens a decision — there is no pending entry
  // for Decline to resolve, and the bytes are already landing. Offering
  // Accept / Trust / Decline there is a rendered consent control for a
  // decision that was never asked and cannot be revoked; tapping it just
  // errors. The persisted status cannot carry this on its own: the engine's
  // `chat_status: transferring` necessarily trails `transfer_started`, so the
  // row still reads `pendingapproval` for that beat.
  testWidgets('under auto-accept the approval controls are never offered — '
      'the decision was never open', (tester) async {
    final fake = FakePeerBeam()..liveTransferIds.add('fr-1');
    fake.chatHistories['pb-bob'] = [
      _file(id: 'fr-1', direction: 'in', status: ChatStatusValue.pendingApproval),
    ];
    await _open(tester, fake);

    // A genuine, still-open offer does show them — otherwise this test would
    // pass by never rendering anything.
    expect(find.text('Accept'), findsOneWidget);
    expect(find.text('Decline'), findsOneWidget);
    expect(find.text('Trust'), findsOneWidget);

    // The engine registers the transfer and starts it in the same breath —
    // that is what auto-accept looks like on the event stream.
    fake.emit(
      const TransferEvent(
        kind: 'transfer_queued',
        transferId: 'fr-1',
        timestamp: '',
        payload: {
          'peer': 'Bob',
          'peer_id': 'pb-bob',
          'file': 'report.pdf',
          'incoming': true,
        },
      ),
    );
    fake.emit(
      const TransferEvent(
        kind: 'transfer_started',
        transferId: 'fr-1',
        timestamp: '',
        payload: {'peer': 'Bob', 'peer_id': 'pb-bob'},
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.text('Accept'), findsNothing);
    expect(find.text('Decline'), findsNothing);
    expect(find.text('Trust'), findsNothing);
    // The row itself is still there — only the dead controls are gone.
    expect(find.text('report.pdf'), findsOneWidget);
  });

  testWidgets('a settled row offers no approval buttons', (tester) async {
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      _file(id: 'fr-1', direction: 'in', status: ChatStatusValue.received),
    ];
    await _open(tester, fake);

    expect(find.text('Accept'), findsNothing);
    expect(find.text('Decline'), findsNothing);
  });

  testWidgets('an in-flight row shows progress from the live transfer, and '
      'still renders when there is none', (tester) async {
    // The engine has the transfer registered, so the row is genuinely in
    // flight — but this process has not seen a progress event for it yet.
    final fake = FakePeerBeam()..liveTransferIds.add('fr-1');
    fake.chatHistories['pb-bob'] = [
      _file(
        id: 'fr-1',
        direction: 'out',
        status: ChatStatusValue.transferring,
      ),
    ];
    await _open(tester, fake);

    // Nothing in TransferRepository yet: an indeterminate bar, not a crash
    // and not a fake 0%.
    LinearProgressIndicator bar() =>
        tester.widget<LinearProgressIndicator>(
          find.byType(LinearProgressIndicator),
        );
    expect(bar().value, isNull);

    // The transfer registers and reports progress under the SAME id.
    fake.emit(
      const TransferEvent(
        kind: 'transfer_queued',
        transferId: 'fr-1',
        timestamp: '',
        payload: {'peer': 'Bob', 'peer_id': 'pb-bob', 'file': 'report.pdf'},
      ),
    );
    fake.emit(
      const TransferEvent(
        kind: 'transfer_progress',
        transferId: 'fr-1',
        timestamp: '',
        payload: {
          'peer_id': 'pb-bob',
          'stats': {'transferred_bytes': 2048, 'total_bytes': 4096},
        },
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(bar().value, closeTo(0.5, 0.001));
  });

  testWidgets('tapping a received file whose recorded copy is gone falls back '
      'to opening it by name through SAF', (tester) async {
    final calls = <MethodCall>[];
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      const MethodChannel('peerbeam/android'),
      (call) async {
        calls.add(call);
        return call.method == 'safOpen' ? true : null;
      },
    );
    final fake = FakePeerBeam();
    fake.chatHistories['pb-bob'] = [
      _file(
        id: 'fr-1',
        direction: 'in',
        status: ChatStatusValue.received,
        // The engine's copy was deleted after the SAF publish: this dangles.
        localPath: '/data/user/0/app/files/report.pdf',
      ),
    ];
    await _open(tester, fake);

    await tester.tap(find.text('report.pdf'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    final saf = calls.where((c) => c.method == 'safOpen');
    expect(saf, hasLength(1));
    expect((saf.single.arguments as Map)['name'], 'report.pdf');
    // The fallback worked, so the user is not told it failed.
    expect(find.byType(SnackBar), findsNothing);
  });
}
