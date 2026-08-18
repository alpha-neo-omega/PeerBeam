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
      // A transfer left a checkpoint behind: it is over, and it is resumable.
      // Always follows its own terminal event, never replaces one.
      case 'transfer_interrupted':
      case 'transfer_discarded':
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
      case 'device_ring':
        final ring = _map(j['payload']);
        final id = ring['device_id'] as String? ?? '';
        return DeviceRing(
          id,
          (ring['device_name'] as String?)?.trim().isNotEmpty == true
              ? ring['device_name'] as String
              : id,
          (ring['seconds'] as num?)?.toInt() ?? 15,
        );
      case 'presence_updated':
        // Live device status from a trusted peer. Payload mirrors one entry of
        // `pb_presence_json`'s `devices` array.
        return PresenceUpdated(
          SdkPresence.fromJson(_map(_map(j['payload'])['device'])),
        );
      case 'clipboard_received':
        // A trusted peer's clipboard. Deliberately NOT `clipboard_updated`,
        // which is the local slot bridge and means "a surface put this here";
        // only this one needs announcing to the user.
        final p = _map(j['payload']);
        return ClipboardReceived(
          deviceId: p['device_id'] as String? ?? '',
          text: p['text'] as String? ?? '',
          sentAt: p['sent_at'] as String? ?? '',
        );
      case 'chat_received':
        return ChatReceived(ChatMessage.fromJson(_map(j['message'])));
      case 'chat_status':
        // `progress` rides the ordinary status event (there is deliberately no
        // second event type for staging), and is absent on every other status.
        final progress = j['progress'];
        return ChatStatus(
          messageId: j['message_id'] as String? ?? '',
          peerId: j['peer_id'] as String? ?? '',
          status: j['status'] as String? ?? '',
          error: j['error'] as String?,
          progress: progress is Map
              ? (
                  done: (progress['done'] as num?)?.toInt() ?? 0,
                  total: (progress['total'] as num?)?.toInt() ?? 0,
                )
              : null,
        );
      default:
        return null;
    }
  }

  static Map<String, dynamic> _map(dynamic v) =>
      v is Map ? Map<String, dynamic>.from(v) : <String, dynamic>{};
}

/// A trusted peer shared (or refreshed) its device status.
class PresenceUpdated extends BridgeEvent {
  final SdkPresence presence;
  const PresenceUpdated(this.presence);
}

/// A trusted peer asked this device to make itself findable — *find my device*.
///
/// The engine has already checked that the asking device holds the presence
/// permission, so a surface's only job is to be noticeable for [seconds]. It is
/// deliberately not told whether we complied: reporting that back would let a
/// caller map which devices are listening.
class DeviceRing extends BridgeEvent {
  /// Who asked, so the surface can say so rather than alarming someone with an
  /// unattributed noise.
  final String deviceId;

  /// The asking device's display name, as this machine recorded it. Falls back
  /// to the id, which is at least specific.
  final String deviceName;

  /// How long to keep signalling. Already clamped by the engine.
  final int seconds;

  const DeviceRing(this.deviceId, this.deviceName, this.seconds);
}

/// A trusted peer synced its clipboard to this device.
///
/// The engine has already validated [text]: bounded, non-empty, valid UTF-8.
/// It is **plain text and must be treated as nothing else** — written to the
/// system clipboard and rendered as characters, never interpreted as markup,
/// a path or a command.
class ClipboardReceived extends BridgeEvent {
  /// The authenticated sender's device id, so a surface can name who changed
  /// the clipboard. A clip that could not be attributed is never delivered.
  final String deviceId;

  /// What the peer copied.
  final String text;

  /// The sender's clock, RFC3339 — display only. Peer clocks are not
  /// synchronised, so nothing branches on it.
  final String sentAt;

  const ClipboardReceived({
    required this.deviceId,
    required this.text,
    required this.sentAt,
  });
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

  /// Local path of the completed item (on `transfer_completed`), or of the
  /// item a checkpoint describes (on `transfer_interrupted`).
  String? get path => payload['path'] as String?;
  bool get incoming => payload['incoming'] == true;

  /// Whether the sending device was pinned by **this very handshake** — i.e.
  /// this is the first time these two devices have ever spoken.
  ///
  /// The one moment that fact is knowable. Trust-on-first-use pins a peer as
  /// it connects, so from the next event onwards the trust store reads the
  /// same for a stranger and for a laptop used daily; only the engine, at
  /// handshake time, can tell them apart. Absent means no — a payload that
  /// does not say so is never treated as first contact.
  bool get newlyTrusted => payload['newly_trusted'] == true;

  /// The session's **pairing code**: a 128-bit safety number derived from both
  /// devices' public keys, in the engine's own grouping (eight groups of four
  /// uppercase hex).
  ///
  /// Both honest peers compute the *same* code; under a man-in-the-middle each
  /// side computes a different one. It is only ever displayed — never compared
  /// here, because this device cannot know what the other screen shows. That
  /// comparison is the user's, out of band, and it is the whole point.
  String get pairingCode => payload['pairing_code'] as String? ?? '';

  /// Direction as the engine spells it (`transfer_interrupted`, whose row is
  /// rebuilt from a checkpoint rather than from a `transfer_queued` this
  /// session saw).
  String? get direction => payload['direction'] as String?;

  /// Whether an interrupted transfer can be restarted from this side. Absent
  /// means no — a Resume that cannot work is worse than none.
  bool get resumable => payload['resumable'] == true;

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

  /// How far a `staging` row's copy has got, when the engine said so.
  ///
  /// Deliberately part of THIS event rather than a new type: staging is a
  /// status a row is *in*, not a different kind of thing happening to it, so a
  /// surface that already routes `chat_status` to a row needs no second
  /// vocabulary to draw its bar. Null on every other status, and null on the
  /// first `staging` event (emitted before a byte has moved).
  ///
  /// The engine throttles these to roughly a hundred over a whole copy — a
  /// 16 GiB stage does not produce 262,000 of them — so they can be rendered
  /// directly. `done` may exceed `total` when the source is being appended to
  /// while it is copied (a log, a download still running), so a bar must clamp
  /// rather than assume.
  final ({int done, int total})? progress;

  const ChatStatus({
    required this.messageId,
    required this.peerId,
    required this.status,
    this.error,
    this.progress,
  });
}
