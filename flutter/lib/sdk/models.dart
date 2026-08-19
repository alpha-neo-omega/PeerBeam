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

/// One device's shared status, as `pb_presence_json` reports it.
///
/// **Every status field is nullable and null means "not shared"**, never zero.
/// A desktop has no battery, and rendering that as `0%` would be a lie the
/// whole optional-field design exists to prevent. Widgets must branch on null,
/// not on a sentinel.
@immutable
class SdkPresence {
  final String deviceId;

  /// 0-100, or null when the device has no battery / did not share one.
  final int? batteryPercent;
  final bool? charging;
  final int? storageFreeBytes;

  /// One of `lan` / `wifi` / `ethernet` / `tailscale` / `unknown`. Already
  /// validated against that set by Rust — an unknown word from a peer arrives
  /// here as null, never verbatim — so it is safe to display.
  final String? network;
  final String? appVersion;

  /// Seconds since **we** received this, not since the peer says it sent it.
  /// Peer clocks are not synchronised.
  final int ageSeconds;

  const SdkPresence({
    required this.deviceId,
    required this.batteryPercent,
    required this.charging,
    required this.storageFreeBytes,
    required this.network,
    required this.appVersion,
    required this.ageSeconds,
  });

  /// Whether this device shared any status at all. False means the tile shows
  /// identity and reachability only — not empty gauges.
  bool get hasAny =>
      batteryPercent != null ||
      storageFreeBytes != null ||
      network != null ||
      appVersion != null;

  factory SdkPresence.fromJson(Map<String, dynamic> j) => SdkPresence(
    deviceId: j['device_id'] as String? ?? '',
    batteryPercent: (j['battery_percent'] as num?)?.toInt(),
    charging: j['charging'] as bool?,
    storageFreeBytes: (j['storage_free_bytes'] as num?)?.toInt(),
    network: j['network'] as String?,
    appVersion: j['app_version'] as String?,
    ageSeconds: (j['age_seconds'] as num?)?.toInt() ?? 0,
  );
}

/// What `pb_presence_json` returns: our own sharing state and every peer's
/// shared status.
@immutable
class PresenceSnapshot {
  /// Whether **this** device is currently sharing its status. Default off.
  final bool sharing;

  /// What this device would share if `sharing` were on — a local preview, so a
  /// user can see what the toggle reveals before flipping it. Never on the wire
  /// while `sharing` is false.
  final SdkPresence self;

  /// Each peer that has shared a status, keyed by device id.
  final Map<String, SdkPresence> devices;

  const PresenceSnapshot({
    required this.sharing,
    required this.self,
    required this.devices,
  });

  static const empty = PresenceSnapshot(
    sharing: false,
    self: SdkPresence(
      deviceId: '',
      batteryPercent: null,
      charging: null,
      storageFreeBytes: null,
      network: null,
      appVersion: null,
      ageSeconds: 0,
    ),
    devices: {},
  );

  factory PresenceSnapshot.fromJson(Map<String, dynamic> j) {
    final list = (j['devices'] as List?) ?? const [];
    final devices = <String, SdkPresence>{};
    for (final e in list) {
      if (e is Map) {
        final p = SdkPresence.fromJson(Map<String, dynamic>.from(e));
        devices[p.deviceId] = p;
      }
    }
    return PresenceSnapshot(
      sharing: j['sharing'] as bool? ?? false,
      self: SdkPresence.fromJson(
        j['self'] is Map
            ? Map<String, dynamic>.from(j['self'] as Map)
            : <String, dynamic>{},
      ),
      devices: devices,
    );
  }
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

/// A transfer whose checkpoint outlived it — one that ended because the link
/// dropped or the app closed, rather than because it finished or was
/// cancelled.
///
/// Deliberately a separate model from [TransferSnapshot] rather than a status
/// on it. An interrupted transfer is not running: nothing will ever emit
/// progress for it, its speed and ETA are meaningless, and — for an inbound
/// one — it cannot be restarted from this side at all. [resumable] carries
/// that last part, and the UI must honour it rather than offer a Resume that
/// would do nothing.
@immutable
class InterruptedTransfer {
  final String id;
  final String direction; // "sending" | "receiving"

  /// The peer's **id**, not its name: a checkpoint outlives the run that made
  /// it, and after a restart there is no name to resolve until discovery finds
  /// the device again.
  final String peerId;
  final String file;
  final String path;
  final int transferredBytes;
  final int totalBytes;
  final String startedAt;

  /// Whether this side can restart it. Only an outgoing transfer can: the
  /// transfer protocol is sender-driven, so an interrupted receive continues
  /// when its sender offers it again.
  final bool resumable;

  const InterruptedTransfer({
    required this.id,
    required this.direction,
    required this.peerId,
    required this.file,
    required this.path,
    required this.transferredBytes,
    required this.totalBytes,
    required this.startedAt,
    required this.resumable,
  });

  bool get sending => direction == 'sending';

  factory InterruptedTransfer.fromJson(Map<String, dynamic> j) =>
      InterruptedTransfer(
        id: j['id'] as String? ?? '',
        direction: j['direction'] as String? ?? 'sending',
        peerId: j['peer_id'] as String? ?? '',
        file: j['file'] as String? ?? '',
        path: j['path'] as String? ?? '',
        transferredBytes:
            (j['stats'] is Map
                    ? (j['stats'] as Map)['transferred_bytes']
                    : j['transferred_bytes'])
                as int? ??
            0,
        totalBytes:
            (j['stats'] is Map
                    ? (j['stats'] as Map)['total_bytes']
                    : j['total_bytes'])
                as int? ??
            0,
        startedAt: j['started_at'] as String? ?? '',
        // Absent means "not resumable": offering a button that cannot work is
        // worse than not offering one.
        resumable: j['resumable'] == true,
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

  /// Whether the **user** chose this device, as opposed to the handshake
  /// having pinned its key.
  ///
  /// Every never-seen peer is pinned as it connects — that pin is what makes a
  /// later key change detectable — so this list includes strangers that merely
  /// reached this device once. Only an approved device may be sent presence,
  /// clipboard contents, or an accepted pipe, so rendering the two alike would
  /// tell the user a stranger is trusted.
  ///
  /// Defaults to `false` for an engine that predates the field: unknown is not
  /// approval.
  final bool approved;

  /// **What** this device may do — the engine's `permissions` array, by name.
  ///
  /// The engine reports the *effective* set, so an unapproved device is always
  /// empty here however its stored record reads: permissions narrow a standing,
  /// they never create one. Rendering is therefore a straight read — a surface
  /// never has to re-derive "but is it approved?".
  ///
  /// Names, not a bitmask, so this list stays meaningful across an engine that
  /// adds a permission: an unknown name simply appears, and
  /// [PeerBeamPermission.all] decides what gets a toggle.
  ///
  /// Empty for an engine that predates the field, which is the fail-closed
  /// reading — a permission this app cannot see is one it must not claim is
  /// granted.
  final Set<String> permissions;

  const TrustedDevice({
    required this.id,
    required this.name,
    required this.fingerprint,
    required this.trustedAt,
    this.approved = false,
    this.permissions = const {},
  });

  /// Whether this device is permitted `permission` (a [PeerBeamPermission]
  /// name).
  bool may(String permission) => permissions.contains(permission);

  factory TrustedDevice.fromJson(Map<String, dynamic> j) => TrustedDevice(
    id: j['id'] as String? ?? '',
    name: j['name'] as String? ?? '',
    fingerprint: j['fingerprint'] as String? ?? '',
    trustedAt:
        DateTime.tryParse(j['trusted_at'] as String? ?? '') ?? DateTime.now(),
    approved: j['approved'] as bool? ?? false,
    permissions: {
      ...?(j['permissions'] as List<dynamic>?)?.whereType<String>(),
    },
  );
}

/// The per-device permission names the engine understands, and how to label
/// them.
///
/// Mirrors `peerbeam_domain::entity::Permission`. Kept as plain strings rather
/// than an enum so an engine that adds a permission does not fail to decode
/// here — a name this build has no label for is simply not offered as a toggle,
/// which is the fail-closed direction for a UI.
abstract final class PeerBeamPermission {
  /// Send and receive file transfers.
  static const files = 'files';

  /// Exchange chat messages.
  static const chat = 'chat';

  /// Receive this device's clipboard while clipboard sync is on.
  static const clipboard = 'clipboard';

  /// Receive this device's status heartbeat while sharing is on.
  static const presence = 'presence';

  /// Have an inbound `peerbeam pipe` accepted by a listening terminal.
  static const pipe = 'pipe';

  /// Exchange notes with this device.
  static const notes = 'notes';

  /// Let this device list the folders you share, read-only.
  static const browse = 'browse';

  /// Every permission this build can render, in the engine's slot order.
  static const all = <String>[
    files,
    chat,
    clipboard,
    presence,
    pipe,
    notes,
    browse,
  ];

  /// The switch label for `permission`.
  static String label(String permission) => switch (permission) {
    files => 'Files',
    chat => 'Messages',
    clipboard => 'Clipboard',
    presence => 'Device status',
    pipe => 'Pipes',
    notes => 'Notes',
    browse => 'Shared folders',
    _ => permission,
  };

  /// What granting `permission` actually allows, in the user's terms — so a
  /// switch is never a word with no consequence attached.
  static String description(String permission) => switch (permission) {
    files => 'Send and receive files with this device',
    chat => 'Send messages to this device',
    clipboard => 'Send this device your clipboard when sync is on',
    presence => 'Send this device your battery, disk and network status',
    pipe => 'Let it pipe data into a listening terminal here',
    notes => 'Keep your notes in sync with this device',
    browse =>
      'Let it list the folders you share (it still needs Files to '
          'receive anything)',
    _ => '',
  };
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
/// One reaction on a message: which emoji, and which side of the conversation
/// put it there.
///
/// `by` is a direction rather than a device id because a conversation has
/// exactly two participants — "mine" and "theirs" is the whole set.
class ChatReaction {
  final String emoji;
  final String by; // 'out' (mine) | 'in' (theirs)
  final DateTime at;

  const ChatReaction({required this.emoji, required this.by, required this.at});

  bool get isMine => by == 'out';

  factory ChatReaction.fromJson(Map<String, dynamic> j) => ChatReaction(
    emoji: j['emoji'] as String? ?? '',
    by: j['by'] as String? ?? 'in',
    at: DateTime.tryParse(j['timestamp'] as String? ?? '') ?? DateTime.now(),
  );
}

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

  /// Reactions on this message, oldest first. Empty — never null — so a
  /// surface never has to distinguish "none" from "not reported".
  final List<ChatReaction> reactions;

  /// When the peer told us it read this message, if it did.
  ///
  /// Only ever set on our own outgoing rows. Null means "not read **or** this
  /// peer does not send receipts" — deliberately indistinguishable, because a
  /// peer that opted out owes no explanation and showing "unread" for it would
  /// be a claim we cannot support.
  final DateTime? readAt;

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
    this.reactions = const [],
    this.readAt,
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
      reactions:
          (j['reactions'] as List?)
              ?.whereType<Map>()
              .map((r) => ChatReaction.fromJson(Map<String, dynamic>.from(r)))
              .toList() ??
          const [],
      readAt: DateTime.tryParse(j['read_at'] as String? ?? ''),
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
    List<ChatReaction>? reactions,
    DateTime? readAt,
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
    reactions: reactions ?? this.reactions,
    readAt: readAt ?? this.readAt,
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

/// One message that matched a search of this device's stored history, and
/// enough to navigate to it.
///
/// It is deliberately not a [ChatMessage]. A hit carries a *snippet* rather
/// than the whole body — that is the point of a bounded search — and has no
/// status and no file metadata, so reusing the message type would mean either
/// shipping every match in full or filling those fields in with values nothing
/// stands behind.
@immutable
class ChatSearchHit {
  /// The conversation the message is in. This is the thread a surface opens
  /// when the hit is tapped, and it comes from the namespace the row was read
  /// from rather than from a field copied out of the row.
  final String peerId;

  /// The message's id within that conversation.
  final String messageId;

  /// When the message was stamped. Best-effort recency for an inbound row,
  /// whose timestamp came off the peer's own clock.
  final DateTime? at;

  /// `'out'` or `'in'`, spelled as [ChatMessage.direction] is.
  final String direction;

  /// One of [ChatMessageKind]: the matched text was this message's body, or —
  /// for a file — its name.
  final String kind;

  /// A **substring of what is stored**: the body around the match for a text
  /// message, the file name for a file. Never re-rendered or reformatted by the
  /// engine, so a surface may highlight the query inside it and be highlighting
  /// the real thing.
  final String snippet;

  const ChatSearchHit({
    required this.peerId,
    required this.messageId,
    required this.at,
    required this.direction,
    required this.kind,
    required this.snippet,
  });

  bool get isMine => direction == 'out';

  bool get isFile => kind == ChatMessageKind.file;

  factory ChatSearchHit.fromJson(Map<String, dynamic> j) => ChatSearchHit(
    peerId: j['peer_id'] as String? ?? '',
    messageId: j['message_id'] as String? ?? '',
    at: DateTime.tryParse(j['timestamp'] as String? ?? ''),
    direction: j['direction'] as String? ?? 'in',
    kind: j['kind'] as String? ?? ChatMessageKind.text,
    snippet: j['snippet'] as String? ?? '',
  );
}

/// What a history search found — and whether that was all there was.
@immutable
/// One note.
///
/// A deleted note is never handed to the app: the engine keeps a tombstone so a
/// deletion can reach other devices, and that is a sync concern, not something
/// a list should render.
class Note {
  final String id;
  final String title;
  final String body;
  final DateTime updatedAt;

  const Note({
    required this.id,
    required this.title,
    required this.body,
    required this.updatedAt,
  });

  /// What a list shows for this note: its title, or its first line when it has
  /// none. Plenty of notes are one line of text and never earn a heading.
  String get heading {
    if (title.isNotEmpty) return title;
    final first = body.split('\n').first.trim();
    return first.isEmpty ? 'Untitled' : first;
  }

  factory Note.fromJson(Map<String, dynamic> j) => Note(
    id: j['id'] as String? ?? '',
    title: j['title'] as String? ?? '',
    body: j['body'] as String? ?? '',
    updatedAt:
        DateTime.tryParse(j['updated_at'] as String? ?? '') ?? DateTime.now(),
  );
}

/// One entry in a remote device's shared folder.
class BrowseEntry {
  final String name;
  final bool isDir;
  final int size;

  const BrowseEntry({
    required this.name,
    required this.isDir,
    required this.size,
  });

  factory BrowseEntry.fromJson(Map<String, dynamic> j) => BrowseEntry(
    name: j['name'] as String? ?? '',
    isDir: j['is_dir'] as bool? ?? false,
    size: (j['size'] as num?)?.toInt() ?? 0,
  );
}

/// A device's answer about one folder.
///
/// [denied] with no entries means the device is not showing this — it may share
/// nothing here, may not have granted this device permission, or the path may
/// not exist. **Those are deliberately indistinguishable**, so a surface must
/// say all three rather than guess.
/// A folder this device offers to peers that hold the Browse permission.
class SharedFolder {
  const SharedFolder({
    required this.name,
    required this.path,
    required this.exists,
  });

  /// What peers address it by — the folder's own directory name.
  final String name;

  /// Where it is on this machine. Shown because two folders called `Documents`
  /// are indistinguishable by name, and nobody should confirm a share they
  /// cannot identify.
  final String path;

  /// Whether the directory is still there. A share whose folder has been moved
  /// or deleted is listed rather than hidden: silently dropping it would leave
  /// someone believing they are sharing something they are not.
  final bool exists;

  factory SharedFolder.fromJson(Map<String, dynamic> json) => SharedFolder(
    name: json['name'] as String? ?? '',
    path: json['path'] as String? ?? '',
    exists: json['exists'] as bool? ?? false,
  );
}

/// One captured log line.
class LogLine {
  const LogLine({
    required this.at,
    required this.level,
    required this.target,
    required this.message,
  });

  /// RFC-3339 timestamp, or empty when the engine could not state one.
  final String at;

  /// `INFO`, `WARN`, `ERROR`, …
  final String level;

  /// The module that emitted it.
  final String target;

  final String message;

  factory LogLine.fromJson(Map<String, dynamic> json) => LogLine(
    at: json['at'] as String? ?? '',
    level: json['level'] as String? ?? '',
    target: json['target'] as String? ?? '',
    message: json['message'] as String? ?? '',
  );

  /// Whether this line reports something going wrong — the reason someone
  /// opened the log at all.
  bool get isProblem => level == 'ERROR' || level == 'WARN';
}

/// What one folder sync did.
class SyncResult {
  const SyncResult({
    required this.fetching,
    required this.pushing,
    required this.deleted,
    required this.renamed,
    required this.conflicts,
    required this.truncated,
  });

  /// Files being fetched from the peer.
  final int fetching;

  /// Files offered to the peer.
  final int pushing;

  /// Files deleted locally because the peer's deletion followed from our copy.
  final int deleted;

  /// Files moved locally instead of re-fetched.
  final int renamed;

  /// Names their copies arrived under because **both** sides changed the file.
  /// The local file was not touched — each entry is a decision the user now has
  /// to make, which is why these are named rather than counted.
  final List<String> conflicts;

  /// Whether the folder held more files than one manifest carries.
  final bool truncated;

  factory SyncResult.fromJson(Map<String, dynamic> json) => SyncResult(
    fetching: (json['fetching'] as num?)?.toInt() ?? 0,
    pushing: (json['pushing'] as num?)?.toInt() ?? 0,
    deleted: (json['deleted'] as num?)?.toInt() ?? 0,
    renamed: (json['renamed'] as num?)?.toInt() ?? 0,
    conflicts: ((json['conflicts'] as List?) ?? const [])
        .map((e) => e.toString())
        .toList(),
    truncated: json['truncated'] as bool? ?? false,
  );

  /// Whether anything at all needs to happen.
  bool get isIdle =>
      fetching == 0 &&
      pushing == 0 &&
      deleted == 0 &&
      renamed == 0 &&
      conflicts.isEmpty;
}

/// A folder being kept in sync continuously.
class WatchedFolder {
  const WatchedFolder({required this.path, required this.into});

  /// The peer's share-relative path.
  final String path;

  /// The local directory it mirrors into.
  final String into;

  factory WatchedFolder.fromJson(Map<String, dynamic> json) => WatchedFolder(
    path: json['path'] as String? ?? '',
    into: json['into'] as String? ?? '',
  );
}

class BrowseListing {
  final String path;
  final List<BrowseEntry> entries;
  final bool truncated;
  final bool denied;

  const BrowseListing({
    required this.path,
    required this.entries,
    required this.truncated,
    required this.denied,
  });

  factory BrowseListing.fromJson(Map<String, dynamic> j) => BrowseListing(
    path: j['path'] as String? ?? '',
    entries: (j['entries'] as List? ?? const [])
        .whereType<Map>()
        .map((e) => BrowseEntry.fromJson(Map<String, dynamic>.from(e)))
        .toList(),
    truncated: j['truncated'] as bool? ?? false,
    denied: j['denied'] as bool? ?? false,
  );
}

/// One thing this device did.
///
/// Carries no message body and no clip text: the timeline says *that* something
/// happened and when, and the conversation and clipboard screens are where you
/// read the content.
class TimelineEvent {
  /// One of `transfer`, `chat`, `clipboard`.
  final String kind;
  final DateTime at;

  /// The other device, or empty when this device acted alone.
  final String peer;

  /// A file name for a transfer or a shared file; empty otherwise.
  final String detail;

  /// Whether it succeeded. Only meaningful for transfers.
  final bool ok;

  const TimelineEvent({
    required this.kind,
    required this.at,
    required this.peer,
    required this.detail,
    required this.ok,
  });

  factory TimelineEvent.fromJson(Map<String, dynamic> j) => TimelineEvent(
    kind: j['kind'] as String? ?? '',
    at: DateTime.tryParse(j['at'] as String? ?? '') ?? DateTime.now(),
    peer: j['peer'] as String? ?? '',
    detail: j['detail'] as String? ?? '',
    ok: j['ok'] as bool? ?? true,
  );
}

/// One remembered clip.
class ClipEntry {
  final String id;
  final String text;

  /// The device that sent it, or null when this device copied it.
  final String? from;
  final DateTime at;

  const ClipEntry({
    required this.id,
    required this.text,
    required this.from,
    required this.at,
  });

  bool get isMine => from == null;

  factory ClipEntry.fromJson(Map<String, dynamic> j) => ClipEntry(
    id: j['id'] as String? ?? '',
    text: j['text'] as String? ?? '',
    from: j['from'] as String?,
    at: DateTime.tryParse(j['at'] as String? ?? '') ?? DateTime.now(),
  );
}

class ChatSearchResults {
  /// Newest first, tie-broken by peer id then message id so the order is stable
  /// between runs.
  final List<ChatSearchHit> hits;

  /// **There were more matches than [limit] allowed.**
  ///
  /// A surface must show this. A bounded search that silently returns its first
  /// `n` reads as "that is all there is", which for a search over the user's own
  /// history is a wrong answer rather than a partial one: the message they are
  /// looking for exists, they have been told it does not, and nothing on screen
  /// suggests asking differently.
  final bool truncated;

  /// The limit the engine actually applied — echoed back, so a surface can say
  /// how many it is showing without having to know whether it passed one.
  final int limit;

  const ChatSearchResults({
    required this.hits,
    required this.truncated,
    required this.limit,
  });

  static const empty = ChatSearchResults(hits: [], truncated: false, limit: 0);

  bool get isEmpty => hits.isEmpty;
}

/// One auto-save rule: a **match**, and a **destination**.
///
/// A rule decides **where** an accepted file is saved. It never decides
/// **whether** it is accepted — that is the approval prompt and the separate
/// "auto-accept trusted devices" setting, neither of which this touches.
///
/// Every criterion is optional and an omitted one matches everything, so a rule
/// with all of them null is a legitimate catch-all. Rules are an **ordered**
/// list and the **first match wins**, which is why the editor lets the user
/// reorder them: that is the only tie-break, and it is one they can see.
@immutable
class SaveRule {
  /// The sending device, by its authenticated id — never the name it presents,
  /// which any peer is free to choose.
  final String? deviceId;

  /// File extension without the leading dot, matched case-insensitively.
  final String? extension;

  /// Inclusive size bounds, in bytes.
  final int? minBytes;
  final int? maxBytes;

  /// The absolute directory a matching file is written to.
  final String directory;

  const SaveRule({
    this.deviceId,
    this.extension,
    this.minBytes,
    this.maxBytes,
    required this.directory,
  });

  /// Whether this rule tests anything at all. A rule that does not is a
  /// catch-all — worth saying out loud in the UI, since one placed above the
  /// others makes every rule below it unreachable.
  bool get isCatchAll =>
      deviceId == null &&
      extension == null &&
      minBytes == null &&
      maxBytes == null;

  factory SaveRule.fromJson(Map<String, dynamic> j) => SaveRule(
    deviceId: j['device'] as String?,
    extension: j['extension'] as String?,
    minBytes: (j['min_bytes'] as num?)?.toInt(),
    maxBytes: (j['max_bytes'] as num?)?.toInt(),
    directory: j['directory'] as String? ?? '',
  );

  /// The engine's shape. An unset criterion is **omitted**, not sent as `""`
  /// or `0`: a blank criterion matches nothing, and `0` is a legitimate
  /// `min_bytes`.
  Map<String, dynamic> toJson() => {
    if (deviceId != null) 'device': deviceId,
    if (extension != null) 'extension': extension,
    if (minBytes != null) 'min_bytes': minBytes,
    if (maxBytes != null) 'max_bytes': maxBytes,
    'directory': directory,
  };

  SaveRule copyWith({String? directory}) => SaveRule(
    deviceId: deviceId,
    extension: extension,
    minBytes: minBytes,
    maxBytes: maxBytes,
    directory: directory ?? this.directory,
  );
}
