// ignore_for_file: prefer_initializing_formals
import 'dart:async';

import 'package:flutter/foundation.dart';

import '../sdk/events.dart';
import '../sdk/models.dart';
import '../sdk/error_text.dart';
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

  /// The live transfer with [id], or null when there is none.
  ///
  /// This map is **ephemeral** — it holds only what is in flight right now and
  /// is empty after a restart — so a caller that renders persisted state (a
  /// chat file row, whose id is its transfer's id) must treat a null as
  /// "no progress to overlay", never as "no such item".
  Transfer? byId(String id) => _byId[id];

  /// User-facing failure messages (surface as a snackbar/notification).
  Stream<String> get errors => _errors.stream;

  /// A clipboard payload arrived (a received `peerbeam-clipboard-*.txt`):
  /// the saved file's path and the sending peer. The UI offers to copy it.
  Stream<({String path, String peer})> get clipboardReceived =>
      _clipboards.stream;
  final StreamController<({String path, String peer})> _clipboards =
      StreamController.broadcast();

  /// A regular file finished downloading: its saved path, file name, and the
  /// sending peer. Used to copy it into the user's chosen folder on platforms
  /// (Android) where the engine's write location isn't user-visible, and to
  /// surface a "Received `name`" notification.
  Stream<({String path, String name, String peer})> get fileReceived =>
      _files.stream;
  final StreamController<({String path, String name, String peer})> _files =
      StreamController.broadcast();

  /// Matches the wire-name convention the sender uses for clipboard sends.
  static final _clipboardName = RegExp(r'^peerbeam-clipboard-\d+\.txt$');

  int get activeCount => _byId.values
      .where(
        (t) =>
            t.state == TransferState.transferring ||
            t.state == TransferState.paused ||
            t.state == TransferState.pending,
      )
      .length;

  void pause(String id) => _api?.pause(id).catchError((_) {});
  void resume(String id) => _api?.resume(id).catchError((_) {});
  void cancel(String id) => _api?.cancel(id).catchError((_) {});

  /// The three approval actions. Unlike pause/resume/cancel these are the
  /// user's **consent**, and they only mean anything while the engine is
  /// actually holding a decision open — the engine answers
  /// `no pending transfer <id>` otherwise, which happens whenever the prompt
  /// has already timed out, the peer went away, or the transfer was
  /// auto-accepted and never asked at all.
  ///
  /// Swallowing that answer is what made a chat file row render live-looking
  /// Accept / Trust / Decline buttons that did nothing when tapped: silence is
  /// indistinguishable from success. It is also the exact shape of this
  /// project's pairing-gate fail-open. So the failure is surfaced, on the same
  /// channel every other transfer failure uses.
  void accept(String id) => _decide(id, 'accept', _api?.accept(id));
  void acceptTrust(String id) =>
      _decide(id, 'accept', _api?.acceptTrust(id));
  void reject(String id) => _decide(id, 'decline', _api?.reject(id));

  void _decide(String id, String verb, Future<void>? call) {
    if (call == null) return;
    unawaited(
      call.catchError((Object e) {
        final what = _byId[id]?.fileName;
        final subject = (what == null || what.isEmpty)
            ? 'this transfer'
            : what;
        _errors.add("Couldn't $verb $subject — ${friendlyError(e)}");
      }),
    );
  }

  /// Send files to a peer; the engine returns ids and drives events.
  Future<void> send(PeerTarget peer, List<String> paths) async {
    await _api?.sendFile(peer, paths);
  }

  /// Send a whole folder to a peer (engine walks it and streams entries).
  Future<void> sendFolder(PeerTarget peer, String path) async {
    await _api?.sendFolder(peer, path);
  }

  void _onEvent(BridgeEvent e) {
    if (e is! TransferEvent) return;
    final id = e.transferId;
    switch (e.kind) {
      case 'transfer_queued':
        _byId[id] = Transfer(
          id: id,
          peerName: e.peer ?? '',
          fileName: e.file ?? e.folder ?? '',
          direction: e.incoming
              ? TransferDirection.receiving
              : TransferDirection.sending,
          state: TransferState.pending,
          // `transfer_queued` carries no stats; the engine now puts the known
          // size on the payload instead (a chat file share knows it up front,
          // and an incoming transfer's first frame is peeked for it). Falling
          // back to it means the size is truthful before the first progress
          // update rather than 0.
          totalBytes: e.stats?.totalBytes ?? e.size ?? 0,
          doneBytes: 0,
        );
      case 'transfer_started':
        _update(id, state: TransferState.transferring);
      case 'transfer_progress':
        final s = e.stats;
        final cur = _byId[id];
        final paused = cur?.state == TransferState.paused;
        _update(
          id,
          state: paused ? TransferState.paused : TransferState.transferring,
          done: s?.transferredBytes,
          total: s?.totalBytes,
          speed: paused ? 0 : s?.currentSpeed,
          eta: paused ? null : s?.etaSecs,
          file: e.file,
        );
      case 'transfer_paused':
        // Freeze the rate readout while paused.
        _update(id, state: TransferState.paused, speed: 0, eta: null);
      case 'transfer_resumed':
        _update(id, state: TransferState.transferring);
      case 'transfer_completed':
        final done = _byId[id];
        if (done != null &&
            done.direction == TransferDirection.receiving &&
            (e.path?.isNotEmpty ?? false)) {
          if (_clipboardName.hasMatch(done.fileName)) {
            _clipboards.add((path: e.path!, peer: done.peerName));
          } else {
            // A real received file — offer it for copy into the user's folder.
            _files.add((
              path: e.path!,
              name: done.fileName,
              peer: done.peerName,
            ));
          }
        }
        _update(id, state: TransferState.completed);
        _byId.remove(id);
      case 'transfer_cancelled':
        _byId.remove(id);
      case 'transfer_failed':
        final name = _byId[id]?.fileName ?? 'Transfer';
        final friendly = friendlyErrorForCode(e.error?.code ?? 'internal');
        _errors.add('$name — $friendly');
        _byId.remove(id);
      default:
        return;
    }
    notifyListeners();
  }

  /// Sentinel for "leave etaSecs unchanged" — distinct from an explicit `null`
  /// (which means "ETA now unknown", e.g. on pause).
  static const Object _unset = Object();

  void _update(
    String id, {
    TransferState? state,
    int? done,
    int? total,
    double? speed,
    Object? eta = _unset,
    String? file,
  }) {
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
      speedBps: speed ?? t.speedBps,
      etaSecs: identical(eta, _unset) ? t.etaSecs : eta as int?,
    );
  }

  @override
  void dispose() {
    _sub?.cancel();
    _errors.close();
    _clipboards.close();
    _files.close();
    super.dispose();
  }
}
