// Typed events decoded from the Rust event stream. One broadcast stream
// carries them all; repositories filter by type.
import 'models.dart';

sealed class BridgeEvent {
  const BridgeEvent();

  /// Decode one event JSON object into a typed event, or null if unrecognised.
  static BridgeEvent? fromJson(Map<String, dynamic> j) {
    final type = j['type'] as String?;
    if (type == null) return null;
    switch (type) {
      // Device (M1) — flat fields.
      case 'device_added':
        return DeviceAdded(SdkDevice.fromJson(_map(j['device'])));
      case 'device_updated':
        return DeviceUpdated(SdkDevice.fromJson(_map(j['device'])));
      case 'device_removed':
        return DeviceRemoved(j['id'] as String? ?? '');
      case 'status_changed':
        return DeviceStatusChanged(
          j['id'] as String? ?? '',
          j['online'] as bool? ?? false,
        );
      case 'latency_changed':
        return DeviceLatencyChanged(
          j['id'] as String? ?? '',
          (j['latency_ms'] as num?)?.toInt(),
        );
      // Transfer (M2) — {transfer_id, timestamp, payload}.
      case 'transfer_queued':
      case 'transfer_started':
      case 'transfer_progress':
      case 'transfer_paused':
      case 'transfer_resumed':
      case 'transfer_retrying':
      case 'transfer_completed':
      case 'transfer_cancelled':
      case 'transfer_failed':
        return TransferEvent(
          kind: type,
          transferId: j['transfer_id'] as String? ?? '',
          timestamp: j['timestamp'] as String? ?? '',
          payload: _map(j['payload']),
        );
      case 'history_updated':
        return const HistoryUpdated();
      case 'trust_changed':
        return const TrustChanged();
      case 'device_resync':
        return const DeviceResync();
      case 'chat_received':
        return ChatReceived(ChatMessage.fromJson(_map(j['message'])));
      case 'chat_status':
        return ChatStatus(
          messageId: j['message_id'] as String? ?? '',
          peerId: j['peer_id'] as String? ?? '',
          status: j['status'] as String? ?? '',
          error: j['error'] as String?,
        );
      default:
        return null;
    }
  }

  static Map<String, dynamic> _map(dynamic v) =>
      v is Map ? Map<String, dynamic>.from(v) : <String, dynamic>{};
}

class DeviceAdded extends BridgeEvent {
  final SdkDevice device;
  const DeviceAdded(this.device);
}

class DeviceUpdated extends BridgeEvent {
  final SdkDevice device;
  const DeviceUpdated(this.device);
}

class DeviceRemoved extends BridgeEvent {
  final String id;
  const DeviceRemoved(this.id);
}

class DeviceStatusChanged extends BridgeEvent {
  final String id;
  final bool online;
  const DeviceStatusChanged(this.id, this.online);
}

class DeviceLatencyChanged extends BridgeEvent {
  final String id;
  final int? latencyMs;
  const DeviceLatencyChanged(this.id, this.latencyMs);
}

/// Any `transfer_*` event. `kind` is the event type; `payload` holds stats etc.
class TransferEvent extends BridgeEvent {
  final String kind;
  final String transferId;
  final String timestamp;
  final Map<String, dynamic> payload;
  const TransferEvent({
    required this.kind,
    required this.transferId,
    required this.timestamp,
    required this.payload,
  });

  TransferStats? get stats {
    final s = payload['stats'];
    return s is Map
        ? TransferStats.fromJson(Map<String, dynamic>.from(s))
        : null;
  }

  String? get file => payload['file'] as String?;

  /// Folder name for a folder-send's `transfer_queued` event, which carries
  /// `folder` rather than `file` (the per-file `file` key only appears once
  /// the walk starts producing entries).
  String? get folder => payload['folder'] as String?;
  String? get peer => payload['peer'] as String?;

  /// The peer's **device id** — the stable, routable identity the display
  /// [peer] name is not. Present on every `transfer_*` event, and what lets a
  /// surface route a transfer to a conversation.
  String? get peerId => payload['peer_id'] as String?;

  /// Total size in bytes, when the engine knows it at `transfer_queued` time
  /// (a chat file share and a peeked incoming transfer both do). The
  /// authoritative running totals still come from [stats].
  int? get size => (payload['size'] as num?)?.toInt();

  /// Local path of the completed item (on `transfer_completed`).
  String? get path => payload['path'] as String?;
  bool get incoming => payload['incoming'] == true;

  ({String code, String message})? get error {
    final e = payload['error'];
    if (e is Map) {
      return (
        code: e['code'] as String? ?? 'internal',
        message: e['message'] as String? ?? '',
      );
    }
    return null;
  }
}

class HistoryUpdated extends BridgeEvent {
  const HistoryUpdated();
}

class TrustChanged extends BridgeEvent {
  const TrustChanged();
}

/// Hint that the device-change stream lagged and dropped transitions; the
/// consumer must re-pull the authoritative device list via `devices()`.
class DeviceResync extends BridgeEvent {
  const DeviceResync();
}

/// A chat message received from a peer.
class ChatReceived extends BridgeEvent {
  final ChatMessage message;
  const ChatReceived(this.message);
}

/// A delivery-status change for a chat row: a queued text message finally
/// delivered, or the terminal status a shared file's transfer settled on.
///
/// [status] is the record's own spelling (see `ChatStatusValue`), so it can be
/// applied straight onto a row read from `chatHistory` with no second
/// vocabulary. It deliberately does NOT carry file metadata — a received
/// file's saved path is written to the persisted record just before the
/// status settles, so re-reading the conversation is what picks it up.
class ChatStatus extends BridgeEvent {
  final String messageId;
  final String peerId;
  final String status;

  /// A human-readable reason, present only when the user is owed an
  /// explanation (a file refused because the peer can't receive chat
  /// attachments, or a send that failed before any byte moved).
  final String? error;

  const ChatStatus({
    required this.messageId,
    required this.peerId,
    required this.status,
    this.error,
  });
}
