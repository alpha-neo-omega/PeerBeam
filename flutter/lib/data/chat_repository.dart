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
  /// `chatSend` performs a full synchronous dial+handshake+send under the
  /// hood (inherent to this increment; a queued outbox lands later), so it
  /// can block briefly. An optimistic outgoing message is appended
  /// immediately — before the await — so the UI feels instant; `refresh`
  /// reconciles with the persisted record once the call resolves. On error
  /// the optimistic message is simply left in place (no retry/outbox yet).
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
      // No outbox/retry in this increment — leave the optimistic message.
    }
  }

  void _onReceived(ChatMessage m) {
    (_byPeer[m.peerId] ??= <ChatMessage>[]).add(m);
    notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _sub?.cancel();
    super.dispose();
  }
}
