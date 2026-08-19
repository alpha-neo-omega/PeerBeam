// ignore_for_file: prefer_initializing_formals
import 'dart:async';

import 'package:flutter/foundation.dart';

import '../sdk/events.dart';
import '../sdk/exceptions.dart';
import '../sdk/models.dart';
import '../sdk/error_text.dart';
import '../sdk/peerbeam.dart';
import '../state/models.dart';

/// What a bulk approval **actually did** — never what it assumed.
///
/// [requested] is how many transfers were waiting when the user tapped;
/// [settled] how many the engine really accepted or declined; [gone] how many
/// had stopped waiting in between (the 180s prompt timed out, the sender gave
/// up, or the user already answered that one from its own card); [failed]
/// anything else that went wrong. `settled + gone + failed == requested`.
typedef BulkDecision = ({int requested, int settled, int gone, int failed});

/// Reactive active-transfer list, driven by engine transfer events. Keeps the
/// UI surface (`transfers`, `activeCount`, `pause`, `resume`, `cancel`) — the
/// commands now go to the engine; state comes back as events.
class TransferRepository extends ChangeNotifier {
  final PeerBeamApi? _api;
  final Map<String, Transfer> _byId = {};
  StreamSubscription<BridgeEvent>? _sub;
  final StreamController<String> _errors = StreamController<String>.broadcast();

  /// Set in [dispose]: an in-flight fetch must not notify a dead listener.
  bool _disposed = false;

  /// True once the interrupted-transfer read has completed, so an empty list
  /// can be told apart from one nobody has fetched yet. A cold start otherwise
  /// renders "No active transfers" before the engine has answered — and a
  /// transfer interrupted by a restart is exactly what that screen exists to
  /// surface.
  bool loaded = false;

  /// Whether the first-contact pairing check is currently on, read live rather
  /// than captured, so toggling it in Settings applies to the prompt already on
  /// screen.
  ///
  /// Advisory only: the engine enforces the check itself and is the authority.
  /// Reading false here while the engine says true costs nothing worse than a
  /// refused accept the user can retry after confirming.
  final bool Function() _pairingConfirmationRequired;

  TransferRepository({
    PeerBeamApi? api,
    bool Function()? pairingConfirmationRequired,
  }) : _api = api,
       _pairingConfirmationRequired =
           pairingConfirmationRequired ?? _checkOffByDefault {
    _sub = _api?.events.listen(_onEvent);
  }

  static bool _checkOffByDefault() => false;

  /// Whether accepting [id] needs the user to confirm a pairing code first:
  /// the transfer is from a device this session pinned, and the check is on.
  ///
  /// The prompt asks this; it never guesses from the transfer alone.
  bool needsPairingConfirmation(String id) =>
      (_byId[id]?.newlyTrusted ?? false) && _pairingConfirmationRequired();

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

  /// The inbound transfers this device has not answered yet — exactly the ones
  /// whose cards show Decline / Accept / Trust, in the order the list renders
  /// them.
  ///
  /// Deliberately narrow: an **outbound** transfer is this device's own send
  /// and was never anyone's to approve, and a transfer already transferring,
  /// paused, completed or failed has had its decision made. Both are excluded
  /// so a bulk action can only ever touch what is genuinely waiting.
  List<Transfer> get awaitingApproval => _byId.values
      .where(
        (t) =>
            t.direction == TransferDirection.receiving &&
            t.state == TransferState.pending,
      )
      .toList(growable: false);

  /// How many transfers are in flight, for the nav badge.
  ///
  /// An interrupted transfer is deliberately not one of them: nothing is
  /// moving, and a badge that counted them would sit permanently lit until the
  /// user cleaned up. `awaitingApproval` excludes them for the sharper reason
  /// that they are not `pending` — nobody is being asked anything.
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

  /// Restart an **interrupted** transfer from its checkpoint.
  ///
  /// Deliberately not [resume], which un-pauses a live one — they are two
  /// different verbs and the engine keeps them as two different calls for
  /// exactly this reason.
  ///
  /// Failure is surfaced rather than swallowed, like the approval actions and
  /// unlike pause/resume/cancel: this one *can* legitimately refuse — the peer
  /// may be unreachable, or the source file may have changed since the
  /// transfer stopped, in which case resuming would append the wrong bytes to
  /// the receiver's partial file. Silence would leave the user tapping a
  /// button that does nothing.
  void resumeInterrupted(String id, {PeerTarget? peer}) =>
      _decide(id, 'resume', _api?.resumeInterrupted(id, peer: peer));

  /// Forget an interrupted transfer and the partial bytes it was holding.
  /// Without it an interrupted transfer is clutter nothing can ever clear.
  void discardInterrupted(String id) =>
      _decide(id, 'discard', _api?.discardInterrupted(id));

  /// Load the transfers whose checkpoints outlived the runs that made them.
  ///
  /// Called after `initialize()`, not from the constructor: an engine call
  /// before init just answers `not_initialised`, and swallowing that would
  /// leave a cold start looking as though nothing had been interrupted.
  ///
  /// Additive — it never removes a row. A transfer that is live right now is
  /// authoritative over any checkpoint bearing its id, so an entry already in
  /// the map is left exactly as it is.
  Future<void> refreshInterrupted() async {
    final api = _api;
    if (api == null) {
      loaded = true;
      notifyListeners();
      return;
    }
    try {
      final rows = await api.interruptedTransfers();
      if (_disposed) return;
      for (final r in rows) {
        if (_byId.containsKey(r.id)) continue;
        _byId[r.id] = _fromCheckpoint(r);
      }
      loaded = true;
      notifyListeners();
    } catch (_) {
      // A transient failure leaves the current view alone rather than
      // blanking it: an empty list is a claim, and this one would be false.
      // The load is still *over*, though — staying "loading" forever is the
      // same lie told the other way round.
      if (_disposed) return;
      loaded = true;
      notifyListeners();
    }
  }

  /// A row for a transfer that only exists as a checkpoint.
  ///
  /// The peer shows as its **id**, because that is all a checkpoint holds: a
  /// name is neither unique nor stable, and after a restart there is nothing
  /// to resolve one against until discovery finds the device again.
  Transfer _fromCheckpoint(InterruptedTransfer r) => Transfer(
    id: r.id,
    peerName: r.peerId,
    fileName: r.file,
    direction: r.sending
        ? TransferDirection.sending
        : TransferDirection.receiving,
    state: TransferState.interrupted,
    totalBytes: r.totalBytes,
    doneBytes: r.transferredBytes,
    resumable: r.resumable,
  );

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
  /// [confirmed] is the user's answer to the first-contact pairing prompt, and
  /// belongs to the surface that actually asked. It is never inferred here: a
  /// repository cannot know whether anyone looked at the other device's screen.
  void accept(String id, {bool confirmed = false}) =>
      _decide(id, 'accept', _api?.accept(id, confirmed: confirmed));
  void acceptTrust(String id, {bool confirmed = false}) =>
      _decide(id, 'accept', _api?.acceptTrust(id, confirmed: confirmed));
  void reject(String id) => _decide(id, 'decline', _api?.reject(id));

  /// Accept every inbound transfer currently [awaitingApproval] — one engine
  /// call per transfer — and report what actually happened.
  ///
  /// **Accept-once. This never trusts.** [acceptTrust] grants a device
  /// persistent auto-accept for everything it sends from now on, which is a
  /// materially stronger and riskier act than approving the batch the user is
  /// looking at; it stays a deliberate, per-device choice on the card. This
  /// path calls [PeerBeamApi.accept] and nothing else.
  ///
  /// Still explicit consent (I6): one tap answers one batch that is on screen
  /// right now. Nothing is remembered and nothing is inferred for next time.
  ///
  /// In substance this is just [acceptOnly] handed every currently-waiting
  /// id — the two are never allowed to answer the question "which ids, and
  /// what happened to them" differently.
  Future<BulkDecision> acceptAll() =>
      acceptOnly(awaitingApproval.map((t) => t.id).toList(growable: false));

  /// Decline every inbound transfer currently [awaitingApproval]. Symmetric
  /// with [acceptAll], including the honest tally.
  Future<BulkDecision> declineAll() =>
      declineOnly(awaitingApproval.map((t) => t.id).toList(growable: false));

  /// Accept exactly [ids] — the transfers the user selected in the Transfers
  /// screen's selection mode — and report what actually happened. Accept-once:
  /// this calls [PeerBeamApi.accept] and never [PeerBeamApi.acceptTrust], for
  /// the same reason [acceptAll] does not.
  ///
  /// [ids] is answered for, not trusted on faith: a selection can be minutes
  /// old, so every id is checked against [awaitingApproval] before anything
  /// is asked of the engine. One that is no longer waiting is counted `gone`
  /// exactly like one that vanishes mid-batch — it is never handed to the
  /// engine as a decision.
  Future<BulkDecision> acceptOnly(List<String> ids) =>
      _decideMany(ids, accepting: true);

  /// Decline exactly [ids]. Symmetric with [acceptOnly].
  Future<BulkDecision> declineOnly(List<String> ids) =>
      _decideMany(ids, accepting: false);

  /// The shared body behind every batch decision — [acceptAll]/[declineAll]
  /// (every currently-waiting id) and [acceptOnly]/[declineOnly] (exactly the
  /// caller's ids) all fall through to this one loop, so the
  /// `InvalidArgumentException` → `gone` classification and the
  /// `settled + gone + failed == requested` invariant can never drift between
  /// entry points that are supposed to agree.
  ///
  /// Unlike the per-card [accept]/[reject] this **awaits** each decision and
  /// counts the answer instead of pushing every refusal onto [errors]: five
  /// accepts where two had already settled would otherwise fire two separate
  /// error snackbars, which reads as breakage rather than as the ordinary race
  /// it is. The caller gets one verified tally and reports it once.
  Future<BulkDecision> _decideMany(
    List<String> ids, {
    required bool accepting,
  }) async {
    if (ids.isEmpty) {
      return (requested: 0, settled: 0, gone: 0, failed: 0);
    }
    final api = _api;
    if (api == null) {
      // No engine to ask, so nothing is known to have stopped waiting —
      // `gone` would be a claim about transfers nobody looked at. Counting
      // them `failed` keeps `settled + gone + failed == requested` true, which
      // `_bulkReport` reads directly: with these ids in neither column it
      // would have said "None were still waiting" about a batch on which no
      // attempt was ever made. Unreachable while `AppState.live` always
      // supplies an api; the invariant is not conditional on that staying so.
      return (requested: ids.length, settled: 0, gone: 0, failed: ids.length);
    }
    // Which of the requested ids are still actually waiting, snapshotted once
    // up front rather than trusted from whenever the caller built [ids]. A
    // stale id — the selection is old, or this is racing an event that just
    // landed — must never reach the engine as a decision; it is exactly as
    // "no longer waiting" as one that fails mid-loop below.
    final waiting = awaitingApproval.map((t) => t.id).toSet();
    var settled = 0, gone = 0, failed = 0;
    for (final id in ids) {
      if (!waiting.contains(id)) {
        gone++;
        continue;
      }
      // A batch cannot confirm a pairing code. One tap answers for every
      // transfer on screen, and comparing a safety number against another
      // device is a per-device act the user has not performed here — so a
      // first-contact transfer is never handed to the engine as a batch accept
      // while the check is on. The engine would refuse it anyway; stopping
      // here is what keeps the tally honest, since its refusal arrives as the
      // same `InvalidArgumentException` that means "no longer waiting", and
      // this one very much still is. Declines are unaffected: refusing needs
      // no verification.
      if (accepting && needsPairingConfirmation(id)) {
        failed++;
        continue;
      }
      try {
        // `accept`, never `acceptTrust` — see acceptAll's contract.
        await (accepting ? api.accept(id) : api.reject(id));
        settled++;
      } on InvalidArgumentException {
        // `no pending transfer <id>`: it stopped waiting between the snapshot
        // above and this call landing. Expected, countable, and not an error
        // to shout about.
        gone++;
      } catch (_) {
        // Something else went wrong. Counted separately so the report can
        // never claim it "was no longer waiting" when that isn't known.
        failed++;
      }
    }
    return (
      requested: ids.length,
      settled: settled,
      gone: gone,
      failed: failed,
    );
  }

  void _decide(String id, String verb, Future<void>? call) {
    if (call == null) return;
    unawaited(
      call.catchError((Object e) {
        final what = _byId[id]?.fileName;
        final subject = (what == null || what.isEmpty) ? 'this transfer' : what;
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
          // First contact, and the code that lets the user check it is the
          // device they think it is. Carried from `transfer_queued` because
          // that is the only event that reports them — the approval prompt
          // this row feeds is rendered long after the handshake is over.
          newlyTrusted: e.newlyTrusted,
          pairingCode: e.pairingCode,
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
      case 'transfer_interrupted':
        // Always arrives *after* the terminal event that removed the row, and
        // says the transfer left something to come back to. Rebuilt from the
        // payload rather than from whatever the row held, because at startup
        // there was never a row: this is the only description of it that
        // exists.
        _byId[id] = Transfer(
          id: id,
          peerName: e.peerId ?? '',
          fileName: e.file ?? '',
          direction: e.direction == 'receiving'
              ? TransferDirection.receiving
              : TransferDirection.sending,
          state: TransferState.interrupted,
          totalBytes: e.stats?.totalBytes ?? 0,
          doneBytes: e.stats?.transferredBytes ?? 0,
          resumable: e.resumable,
        );
      case 'transfer_discarded':
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
    _disposed = true;
    _sub?.cancel();
    _errors.close();
    _clipboards.close();
    _files.close();
    super.dispose();
  }
}
