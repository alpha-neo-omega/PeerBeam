// Immutable SDK models — the shapes the Rust engine sends over FFI (JSON DTOs).
// The app maps these to its own UI models where needed.
import 'package:flutter/foundation.dart';

@immutable
class SdkDevice {
  final String id;
  final String name;
  final String kind;
  final String platform;
  final List<String> addresses;
  final int port;
  final bool online;
  final int? latencyMs;
  final bool reachableLan;
  final bool reachableRemote;

  const SdkDevice({
    required this.id,
    required this.name,
    required this.kind,
    required this.platform,
    required this.addresses,
    required this.port,
    required this.online,
    required this.latencyMs,
    required this.reachableLan,
    required this.reachableRemote,
  });

  factory SdkDevice.fromJson(Map<String, dynamic> j) => SdkDevice(
    id: j['id'] as String,
    name: j['name'] as String? ?? 'Device',
    kind: j['kind'] as String? ?? 'desktop',
    platform: j['platform'] as String? ?? 'linux',
    addresses:
        (j['addresses'] as List?)?.map((e) => e as String).toList() ?? const [],
    port: (j['port'] as num?)?.toInt() ?? 0,
    online: j['online'] as bool? ?? false,
    latencyMs: (j['latency_ms'] as num?)?.toInt(),
    reachableLan: j['reachable_lan'] as bool? ?? false,
    reachableRemote: j['reachable_remote'] as bool? ?? false,
  );
}

@immutable
class TransferStats {
  final int transferredBytes;
  final int totalBytes;
  final double currentSpeed;
  final double averageSpeed;
  final int? etaSecs;

  const TransferStats({
    required this.transferredBytes,
    required this.totalBytes,
    required this.currentSpeed,
    required this.averageSpeed,
    required this.etaSecs,
  });

  static const empty = TransferStats(
    transferredBytes: 0,
    totalBytes: 0,
    currentSpeed: 0,
    averageSpeed: 0,
    etaSecs: null,
  );

  double get progress =>
      totalBytes == 0 ? 0 : (transferredBytes / totalBytes).clamp(0, 1);

  factory TransferStats.fromJson(Map<String, dynamic> j) => TransferStats(
    transferredBytes: (j['transferred_bytes'] as num?)?.toInt() ?? 0,
    totalBytes: (j['total_bytes'] as num?)?.toInt() ?? 0,
    currentSpeed: (j['current_speed'] as num?)?.toDouble() ?? 0,
    averageSpeed: (j['average_speed'] as num?)?.toDouble() ?? 0,
    etaSecs: (j['eta_secs'] as num?)?.toInt(),
  );
}

@immutable
class TransferSnapshot {
  final String id;
  final String direction; // "sending" | "receiving"
  final String peer;
  final String file;
  final String status;
  final TransferStats stats;

  const TransferSnapshot({
    required this.id,
    required this.direction,
    required this.peer,
    required this.file,
    required this.status,
    required this.stats,
  });

  bool get sending => direction == 'sending';

  factory TransferSnapshot.fromJson(Map<String, dynamic> j) => TransferSnapshot(
    id: j['id'] as String,
    direction: j['direction'] as String? ?? 'sending',
    peer: j['peer'] as String? ?? '',
    file: j['file'] as String? ?? '',
    status: j['status'] as String? ?? 'queued',
    stats: j['stats'] is Map
        ? TransferStats.fromJson(Map<String, dynamic>.from(j['stats'] as Map))
        : TransferStats.empty,
  );

  TransferSnapshot copyWith({
    String? status,
    TransferStats? stats,
    String? file,
  }) => TransferSnapshot(
    id: id,
    direction: direction,
    peer: peer,
    file: file ?? this.file,
    status: status ?? this.status,
    stats: stats ?? this.stats,
  );
}

@immutable
class HistoryEntry {
  final String id;
  final String direction;
  final String peer;
  final String file;

  /// Local path of the item (source for sends, saved location for receives).
  /// Empty when the engine predates path recording.
  final String path;
  final int bytes;
  final bool success;
  final String at;

  const HistoryEntry({
    required this.id,
    required this.direction,
    required this.peer,
    required this.file,
    required this.path,
    required this.bytes,
    required this.success,
    required this.at,
  });

  factory HistoryEntry.fromJson(Map<String, dynamic> j) => HistoryEntry(
    id: j['id'] as String? ?? '',
    direction: j['direction'] as String? ?? 'sending',
    peer: j['peer'] as String? ?? '',
    file: j['file'] as String? ?? '',
    path: j['path'] as String? ?? '',
    bytes: (j['bytes'] as num?)?.toInt() ?? 0,
    success: j['success'] as bool? ?? false,
    at: j['at'] as String? ?? '',
  );
}

/// A peer target for a send (matches the FFI `peer` JSON).
///
/// [id] is the discovered/saved device's stable id, when known. It is
/// nullable because one construction site (the manual host/port "Send to
/// address" dialog) has no device id to offer — the engine's `device_from`
/// falls back to a placeholder id in that case. Whenever a real id is
/// available it must be threaded through, since the engine keys
/// conversation/session state (e.g. chat history) by this id, not by name.
@immutable
class PeerTarget {
  final String name;
  final List<String> addresses;
  final int port;
  final String? id;
  const PeerTarget({
    required this.name,
    required this.addresses,
    required this.port,
    this.id,
  });

  Map<String, dynamic> toJson() {
    final m = <String, dynamic>{
      'name': name,
      'addresses': addresses,
      'port': port,
    };
    if (id != null) m['id'] = id;
    return m;
  }
}

/// A pinned (trusted) peer, as recorded by the engine's TOFU store.
class TrustedDevice {
  final String id;
  final String name;
  final String fingerprint;
  final DateTime trustedAt;

  const TrustedDevice({
    required this.id,
    required this.name,
    required this.fingerprint,
    required this.trustedAt,
  });

  factory TrustedDevice.fromJson(Map<String, dynamic> j) => TrustedDevice(
    id: j['id'] as String? ?? '',
    name: j['name'] as String? ?? '',
    fingerprint: j['fingerprint'] as String? ?? '',
    trustedAt:
        DateTime.tryParse(j['trusted_at'] as String? ?? '') ?? DateTime.now(),
  );
}

/// What a chat record holds, as the engine serializes `peerbeam_chat::Kind`.
abstract final class ChatMessageKind {
  /// A text message — and every record written before file-in-chat, which has
  /// no `kind` key at all (the Rust side defaults it, and so do we).
  static const text = 'text';

  /// A file shared inside the conversation. Its `body` is empty; the file's
  /// name/size/local path live under [ChatMessage.fileName] etc.
  static const file = 'file';
}

/// The delivery/lifecycle statuses of a chat record, spelled exactly as the
/// engine serializes `peerbeam_chat::Status`.
///
/// That enum carries `#[serde(rename_all = "lowercase")]`, which lowercases
/// the whole variant name **without inserting a separator** — so `PendingApproval`
/// is on the wire as `pendingapproval`, not `pendingApproval` and not
/// `pending_approval`. The same spellings come back on a `chat_status` event
/// (Rust pins the two together in `chat_status_str`), so a surface can apply an
/// event's status straight onto a row it read from `pb_chat_history`.
abstract final class ChatStatusValue {
  /// Queued in the engine's offline outbox, waiting for the peer.
  ///
  /// A **file** row reads this once its bytes are staged and its queue entry
  /// written (increment 2b) — the file has not been delivered, and rendering it
  /// as sent would be a lie. See `_statusLabel` in the chat screen, which spells
  /// it "Queued" for a file and "Waiting" for text.
  static const pending = 'pending';

  /// A file whose bytes are being copied into the outbox's own storage. Nothing
  /// has been queued or offered yet, so nothing can settle it.
  ///
  /// The first state an outgoing file share is persisted in, and a slow one —
  /// a multi-GB copy takes real time, and the engine reports how far it has got
  /// on the same `chat_status` event under a `progress` object (`ChatStatus`
  /// in `events.dart`). A surface must show that rather than an attach that
  /// appears to hang.
  static const staging = 'staging';

  /// Delivered to the peer.
  static const sent = 'sent';

  /// Received from the peer.
  static const received = 'received';

  /// A file the peer is offering us, awaiting the user's accept/decline.
  static const pendingApproval = 'pendingapproval';

  /// A file whose bytes are moving.
  static const transferring = 'transferring';

  /// The peer (or we) turned the file down.
  static const declined = 'declined';

  /// The transfer failed.
  static const failed = 'failed';

  /// Left mid-flight by a crash/restart; no event will ever complete it.
  static const interrupted = 'interrupted';
}

/// A single chat message, sent or received via a [PeerTarget].
///
/// A record is either text or a shared file ([kind]); both ride the same
/// conversation and the same status vocabulary. A file record's [body] is
/// **empty** — rendering it as text produces a blank bubble — and its metadata
/// arrives under the record's `file` object.
@immutable
class ChatMessage {
  final String id;
  final String peerId;
  final String direction; // 'out' | 'in'
  final String body;
  final DateTime at;

  /// One of [ChatStatusValue].
  final String status;

  /// One of [ChatMessageKind]. Defaults to text, so a record persisted before
  /// file-in-chat (no `kind` key) reads exactly as it always did.
  final String kind;

  /// File metadata, all null unless [isFile].
  final String? fileName;
  final int? fileSize;

  /// Where the file lives on THIS device: the source path on the sender, the
  /// saved path on the receiver. Null until a receive completes.
  ///
  /// On Android it can dangle: the engine's private copy is deleted once the
  /// file has been published into the user's SAF folder, so an "open" must be
  /// prepared to fall back to opening it by [fileName].
  final String? localPath;

  const ChatMessage({
    required this.id,
    required this.peerId,
    required this.direction,
    required this.body,
    required this.at,
    required this.status,
    this.kind = ChatMessageKind.text,
    this.fileName,
    this.fileSize,
    this.localPath,
  });

  bool get isMine => direction == 'out';

  /// Whether this row is a shared file rather than text.
  bool get isFile => kind == ChatMessageKind.file;

  /// A file the peer is offering us that still needs a decision. The row's id
  /// is also the transfer id, so the existing accept/trust/reject calls take
  /// it unchanged.
  bool get awaitingApproval =>
      isFile && !isMine && status == ChatStatusValue.pendingApproval;

  factory ChatMessage.fromJson(Map<String, dynamic> j) {
    // `file` is absent on a legacy record and an explicit null on every text
    // record; neither may be mistaken for metadata.
    final raw = j['file'];
    final file = raw is Map ? Map<String, dynamic>.from(raw) : null;
    return ChatMessage(
      id: j['id'] as String? ?? '',
      peerId: j['peer_id'] as String? ?? '',
      direction: j['direction'] as String? ?? 'in',
      body: j['body'] as String? ?? '',
      at: DateTime.tryParse(j['timestamp'] as String? ?? '') ?? DateTime.now(),
      status: j['status'] as String? ?? ChatStatusValue.received,
      kind: j['kind'] as String? ?? ChatMessageKind.text,
      fileName: file?['name'] as String?,
      fileSize: (file?['size'] as num?)?.toInt(),
      localPath: file?['local_path'] as String?,
    );
  }

  /// A copy with the named fields replaced; every omitted field is carried
  /// over unchanged (an omitted argument never *clears* a field).
  ///
  /// [kind], [fileName] and [fileSize] are settable because a row can learn
  /// what it is after it was created: an optimistic staging row is appended
  /// before the engine has answered, so its size — and, for a share the picker
  /// only knew a path for, its name — arrive late.
  ChatMessage copyWith({
    String? status,
    String? localPath,
    String? kind,
    String? fileName,
    int? fileSize,
  }) => ChatMessage(
    id: id,
    peerId: peerId,
    direction: direction,
    body: body,
    at: at,
    status: status ?? this.status,
    kind: kind ?? this.kind,
    fileName: fileName ?? this.fileName,
    fileSize: fileSize ?? this.fileSize,
    localPath: localPath ?? this.localPath,
  );
}

/// One conversation this device holds, as `pb_chat_conversations` reports it.
///
/// This list is what makes a thread reachable for a peer discovery cannot
/// currently see — the engine derives it from the conversation namespaces that
/// exist on disk, not from anything on the network.
///
/// [peerId] is the peer's **authenticated device id**, the same id every record
/// in the thread is filed under. It is the only id a conversation may be opened
/// by: a locally minted one (a saved device's timestamp id, say) produces a
/// write-only thread whose replies land somewhere else entirely.
@immutable
class ChatConversation {
  final String peerId;

  /// When the newest record in the thread was stamped, or null for a thread
  /// this build could not read. Such a thread is still listed — dropping it
  /// would hide the very conversation this list exists to make reachable.
  ///
  /// Best-effort recency only: an inbound record's timestamp came off the
  /// peer's own clock.
  final DateTime? lastAt;

  /// How many **inbound file offers in this thread are still awaiting the
  /// user's decision** — rows with a live Accept button.
  ///
  /// It is NOT an unread-message count, and must never be rendered as one.
  /// PeerBeam has no read receipts and records nothing about when a thread was
  /// last opened (by charter — no telemetry, no cross-device state), so an
  /// "N unread" would be a guess presented as a fact: a user who read every
  /// message and simply did not reply would be badged as having ignored them.
  /// What this counts is instead something the store actually knows and the
  /// user can act on — *this thread is waiting on you for n decisions*.
  ///
  /// The cost is explicit and accepted: a thread full of unread **text** reads
  /// 0. See [needsAttention].
  final int unreadHint;

  const ChatConversation({
    required this.peerId,
    required this.lastAt,
    required this.unreadHint,
  });

  /// Whether this thread is waiting on the user for a decision — the only
  /// claim [unreadHint] supports.
  bool get needsAttention => unreadHint > 0;

  factory ChatConversation.fromJson(Map<String, dynamic> j) => ChatConversation(
    peerId: j['peer_id'] as String? ?? '',
    // Absent, explicitly null, or unparseable all mean the same thing here:
    // nothing is known about when this thread last moved.
    lastAt: DateTime.tryParse(j['last_timestamp'] as String? ?? ''),
    unreadHint: (j['unread_hint'] as num?)?.toInt() ?? 0,
  );
}
