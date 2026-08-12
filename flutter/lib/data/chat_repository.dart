// ignore_for_file: prefer_initializing_formals
import 'dart:async';

import 'package:flutter/foundation.dart';

import '../sdk/events.dart';
import '../sdk/exceptions.dart';
import '../sdk/models.dart';
import '../sdk/peerbeam.dart';

/// Per-peer chat conversations, refetched from the engine on demand and
/// appended-to live as `chat_received` events arrive.
///
/// Conversations are keyed by the peer's real device id, passed explicitly
/// as `peerId` alongside the [PeerTarget] used to actually send. [PeerTarget]
/// does carry its own `id` field now, but it's optional — a manually-entered
/// host:port target has none — so callers (the chat screen) still thread the
/// device id through separately rather than reading it off [PeerTarget].
class ChatRepository extends ChangeNotifier {
  final PeerBeamApi? _api;
  final Map<String, List<ChatMessage>> _byPeer = {};

  /// Why a row failed, keyed by message id. Session-scoped on purpose: the
  /// engine persists the failed *status* but not a reason, and the reason
  /// arrives only on the `chat_status` event that reports it.
  final Map<String, String> _errors = {};

  /// Rows the engine deliberately never persisted, per peer.
  ///
  /// `chat_send_file` validates the path *before* it writes anything, so a
  /// refused one (a folder, a file that moved, an over-long name) leaves no
  /// record at all — and [refresh] rebuilds a conversation from engine history
  /// alone. Without this, the "it never left" bubble would be erased by the
  /// very next reconcile: a sibling in the same multi-select, the next text
  /// message, an incoming file settling, or simply reopening the thread. They
  /// are re-appended by [refresh] and cleared only by [dismiss].
  final Map<String, List<ChatMessage>> _unsent = {};
  StreamSubscription<BridgeEvent>? _sub;
  bool _disposed = false;
  int _optimisticSeq = 0;

  /// Note: does NOT refresh in the constructor — same reasoning as
  /// `TrustRepository`/`HistoryRepository`. Repositories are constructed
  /// synchronously in `AppState.live` during `initState`, before the engine's
  /// `initialize()` has been awaited; an early `refresh()` would just hit
  /// `not_initialised` and be swallowed. Chat is additionally per-peer, so
  /// there is no single boot-time refresh anyway — the chat screen refreshes
  /// its own conversation when it opens.
  ChatRepository({PeerBeamApi? api}) : _api = api {
    _sub = _api?.events.listen((e) {
      if (e is ChatReceived) _onReceived(e.message);
      if (e is ChatStatus) _onStatus(e);
    });
  }

  /// Messages for one conversation, oldest first (as returned by the engine).
  List<ChatMessage> messagesFor(String peerId) =>
      List.unmodifiable(_byPeer[peerId] ?? const []);

  /// Why the message with [messageId] failed, when the engine said so. Null
  /// for every message that hasn't failed (and for a failure this session
  /// never saw — the reason isn't persisted).
  String? errorFor(String messageId) => _errors[messageId];

  /// Whether this row exists only in this session because the engine refused
  /// to send it — nothing was persisted, so the user is the only one who can
  /// clear it (see [dismiss]).
  bool isUnsent(String peerId, String messageId) =>
      _unsent[peerId]?.any((m) => m.id == messageId) ?? false;

  /// Acknowledge an unsent row: drop it from the conversation for good.
  void dismiss(String peerId, String messageId) {
    _unsent[peerId]?.removeWhere((m) => m.id == messageId);
    if (_unsent[peerId]?.isEmpty ?? false) _unsent.remove(peerId);
    _errors.remove(messageId);
    _byPeer[peerId]?.removeWhere((m) => m.id == messageId);
    notifyListeners();
  }

  /// Pull the persisted conversation with [peerId] from the engine.
  Future<void> refresh(String peerId) async {
    final api = _api;
    if (api == null) return;
    try {
      final msgs = await api.chatHistory(peerId);
      if (_disposed) return;
      // Copied, not stored by reference: this repository appends to its own
      // lists (optimistic rows, live `chat_received`), so holding the SDK's
      // list would both mutate the caller's data and break outright on a
      // fixed-length/const one.
      //
      // Engine history is authoritative for everything it knows about, but it
      // cannot know about a share the engine refused before persisting
      // anything — so those rows are carried across, newest last (they were
      // created just now, and nothing will ever deliver them). See [_unsent].
      _byPeer[peerId] = [...msgs, ...?_unsent[peerId]];
      notifyListeners();
    } catch (_) {
      // Keep the current view on transient errors.
    }
  }

  /// Send [text] to [peer], filed under [peerId] in the conversation map.
  ///
  /// `chatSend` enqueues the message durably and returns immediately (1b:
  /// offline-first send) — it does not block on a dial/handshake. Delivery
  /// happens via an opportunistic flush right after enqueueing, plus a
  /// background drain/flush-on-connect that keeps retrying indefinitely
  /// while the peer stays unreachable. An optimistic outgoing message is
  /// appended immediately — before the await — so the UI feels instant;
  /// `refresh` reconciles with the persisted record once the call resolves,
  /// and a `chat_status` event later flips the message's status in place
  /// (via [_onStatus]) once it's actually delivered.
  Future<void> send(String peerId, PeerTarget peer, String text) async {
    final body = text.trim();
    if (body.isEmpty) return;
    final optimistic = ChatMessage(
      id: 'local-${++_optimisticSeq}',
      peerId: peerId,
      direction: 'out',
      body: body,
      at: DateTime.now(),
      status: 'pending',
    );
    (_byPeer[peerId] ??= <ChatMessage>[]).add(optimistic);
    notifyListeners();
    try {
      await _api?.chatSend(peer, body);
      await refresh(peerId);
    } catch (_) {
      // chatSend itself only fails on a local/validation error (enqueueing
      // is durable and always attempted) — an unreachable peer is not an
      // error here, it stays queued in the engine's outbox and is retried by
      // the drain/flush-on-connect until delivered. Leave the optimistic
      // message in place; `_onStatus` will update it once delivery happens.
    }
  }

  /// Share the file at [path] inside the conversation with [peer].
  ///
  /// Mirrors [send], with one deliberate difference: **there is no outbox**.
  /// `chatSendFile` is online-only (increment 2a), so a peer that cannot be
  /// reached fails the row rather than promising a later delivery — and a
  /// failure is therefore worth showing, not swallowing.
  ///
  /// [name] and [size] are only used for the optimistic row shown before the
  /// engine answers; the persisted record is authoritative from `refresh` on.
  /// The engine validates and persists the row synchronously (before it dials
  /// anything), so that reconcile always finds it.
  ///
  /// Call this once per picked file. A multi-select fans out here, never at
  /// the engine — sending only the first of several files the user chose is
  /// silent data loss.
  Future<void> sendFile(
    String peerId,
    PeerTarget peer,
    String path, {
    String? name,
    int? size,
  }) async {
    if (path.isEmpty) return;
    final id = 'local-${++_optimisticSeq}';
    (_byPeer[peerId] ??= <ChatMessage>[]).add(
      ChatMessage(
        id: id,
        peerId: peerId,
        direction: 'out',
        // A file row carries no text — the engine persists an empty body too.
        body: '',
        at: DateTime.now(),
        status: ChatStatusValue.transferring,
        kind: ChatMessageKind.file,
        fileName: name ?? _basename(path),
        fileSize: size,
        localPath: path,
      ),
    );
    notifyListeners();
    try {
      await _api?.chatSendFile(peer, path);
      await refresh(peerId);
    } on PeerBeamException catch (e) {
      // The engine refused the path itself (missing file, a folder): nothing
      // was persisted and nothing was sent, so a `refresh` here would silently
      // erase the row the user is looking at. Fail it in place instead, with
      // the engine's own reason.
      _fail(peerId, id, e.message);
    } catch (_) {
      // Anything else (a malformed reply, a dead engine): still the user's
      // row to account for. This call is fire-and-forget from the attach
      // button, so an escaping error would be an unhandled async one.
      _fail(peerId, id, 'Could not share ${name ?? _basename(path)}');
    }
  }

  /// Mark a message failed in place, remember why, and — because the engine
  /// persisted nothing for it — remember the row itself so no later reconcile
  /// can quietly erase the fact that it never left.
  void _fail(String peerId, String messageId, String reason) {
    _errors[messageId] = reason;
    final list = _byPeer[peerId];
    final i = list?.indexWhere((m) => m.id == messageId) ?? -1;
    if (list != null && i >= 0) {
      final failed = list[i].copyWith(status: ChatStatusValue.failed);
      list[i] = failed;
      (_unsent[peerId] ??= <ChatMessage>[]).add(failed);
    }
    notifyListeners();
  }

  static String _basename(String path) {
    final norm = path.replaceAll('\\', '/');
    final i = norm.lastIndexOf('/');
    return i >= 0 ? norm.substring(i + 1) : norm;
  }

  void _onReceived(ChatMessage m) {
    (_byPeer[m.peerId] ??= <ChatMessage>[]).add(m);
    notifyListeners();
  }

  /// Flip a message's status in place: `pending` → `sent` once the engine's
  /// outbox delivers a text message, or the terminal status a shared file's
  /// transfer settled on. A safe no-op when the peer or message id isn't known
  /// locally — e.g. a stale/late status event for a conversation this session
  /// never loaded.
  ///
  /// This is the ONLY thing that drives a chat row's status. The engine
  /// deliberately gates which rows a transfer may settle (right conversation,
  /// right row, still in flight) before emitting this, so deriving statuses
  /// here from raw `transfer_*` events as well would be a second, ungated path
  /// to the same state — the live [TransferRepository] entry is used purely to
  /// overlay progress on an in-flight row.
  void _onStatus(ChatStatus e) {
    // A status without a reason supersedes any earlier one: the row moved on,
    // so a stale explanation must not stay under it.
    final reason = e.error;
    if (reason != null) {
      _errors[e.messageId] = reason;
    } else {
      _errors.remove(e.messageId);
    }
    final list = _byPeer[e.peerId];
    if (list == null) return;
    final i = list.indexWhere((m) => m.id == e.messageId);
    if (i < 0) return;
    final updated = list[i].copyWith(status: e.status);
    list[i] = updated;
    notifyListeners();
    // A received file's saved location is written onto the persisted record
    // just before the row settles, and is not carried by this event — re-read
    // the conversation so the row can offer "Open". Narrow on purpose: no
    // other status transition adds anything a re-read would find.
    if (updated.isFile && e.status == ChatStatusValue.received) {
      unawaited(refresh(e.peerId));
    }
  }

  @override
  void dispose() {
    _disposed = true;
    _sub?.cancel();
    super.dispose();
  }
}
