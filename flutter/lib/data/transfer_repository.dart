// ignore_for_file: prefer_initializing_formals
import 'dart:async';

import 'package:flutter/foundation.dart';

import '../sdk/events.dart';
import '../sdk/models.dart';
import '../sdk/peerbeam.dart';
import '../state/models.dart';

/// Reactive active-transfer list, driven by engine transfer events. Keeps the
/// UI surface (`transfers`, `activeCount`, `pause`, `resume`, `cancel`) — the
/// commands now go to the engine; state comes back as events.
class TransferRepository extends ChangeNotifier {
  final PeerBeamApi? _api;
  final Map<String, Transfer> _byId = {};
  StreamSubscription<BridgeEvent>? _sub;
  final StreamController<String> _errors = StreamController<String>.broadcast();

  TransferRepository({PeerBeamApi? api}) : _api = api {
    _sub = _api?.events.listen(_onEvent);
  }

  List<Transfer> get transfers => List.unmodifiable(_byId.values);

  /// User-facing failure messages (surface as a snackbar/notification).
  Stream<String> get errors => _errors.stream;

  int get activeCount => _byId.values
      .where((t) =>
          t.state == TransferState.transferring ||
          t.state == TransferState.paused ||
          t.state == TransferState.pending)
      .length;

  void pause(String id) => _api?.pause(id).catchError((_) {});
  void resume(String id) => _api?.resume(id).catchError((_) {});
  void cancel(String id) => _api?.cancel(id).catchError((_) {});
  void accept(String id) => _api?.accept(id).catchError((_) {});
  void reject(String id) => _api?.reject(id).catchError((_) {});

  /// Send files to a peer; the engine returns ids and drives events.
  Future<void> send(PeerTarget peer, List<String> paths) async {
    await _api?.sendFile(peer, paths);
  }

  void _onEvent(BridgeEvent e) {
    if (e is! TransferEvent) return;
    final id = e.transferId;
    switch (e.kind) {
      case 'transfer_queued':
        _byId[id] = Transfer(
          id: id,
          peerName: e.peer ?? '',
          fileName: e.file ?? '',
          direction: e.incoming
              ? TransferDirection.receiving
              : TransferDirection.sending,
          state: TransferState.pending,
          totalBytes: e.stats?.totalBytes ?? 0,
          doneBytes: 0,
        );
      case 'transfer_started':
        _update(id, state: TransferState.transferring);
      case 'transfer_progress':
        final s = e.stats;
        _update(
          id,
          state: TransferState.transferring,
          done: s?.transferredBytes,
          total: s?.totalBytes,
          file: e.file,
        );
      case 'transfer_paused':
        _update(id, state: TransferState.paused);
      case 'transfer_resumed':
        _update(id, state: TransferState.transferring);
      case 'transfer_completed':
        _update(id, state: TransferState.completed);
        _byId.remove(id);
      case 'transfer_cancelled':
        _byId.remove(id);
      case 'transfer_failed':
        final name = _byId[id]?.fileName ?? 'Transfer';
        final msg = e.error?.message ?? 'failed';
        _errors.add('$name failed: $msg');
        _byId.remove(id);
      default:
        return;
    }
    notifyListeners();
  }

  void _update(String id,
      {TransferState? state, int? done, int? total, String? file}) {
    final t = _byId[id];
    if (t == null) return;
    _byId[id] = Transfer(
      id: t.id,
      peerName: t.peerName,
      fileName: file ?? t.fileName,
      direction: t.direction,
      state: state ?? t.state,
      totalBytes: total ?? t.totalBytes,
      doneBytes: done ?? t.doneBytes,
    );
  }

  @override
  void dispose() {
    _sub?.cancel();
    _errors.close();
    super.dispose();
  }
}
