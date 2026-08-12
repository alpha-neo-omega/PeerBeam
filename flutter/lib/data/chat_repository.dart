// ignore_for_file: prefer_initializing_formals
import 'dart:async';

import 'package:flutter/foundation.dart';

import '../sdk/events.dart';
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

  /// Pull the persisted conversation with [peerId] from the engine.
  Future<void> refresh(String peerId) async {
    final api = _api;
    if (api == null) return;
    try {
      final msgs = await api.chatHistory(peerId);
      if (_disposed) return;
      _byPeer[peerId] = msgs;
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

  void _onReceived(ChatMessage m) {
    (_byPeer[m.peerId] ??= <ChatMessage>[]).add(m);
    notifyListeners();
  }

  /// Flip a previously-sent message's status in place (e.g. `pending` →
  /// `sent`, once the engine's outbox actually delivers it). A safe no-op
  /// when the peer or message id isn't known locally — e.g. a stale/late
  /// status event for a conversation this session never loaded.
  void _onStatus(ChatStatus e) {
    final list = _byPeer[e.peerId];
    if (list == null) return;
    final i = list.indexWhere((m) => m.id == e.messageId);
    if (i < 0) return;
    list[i] = list[i].copyWith(status: e.status);
    notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _sub?.cancel();
    super.dispose();
  }
}
