import 'package:flutter/material.dart';

/// View models for the UI. Repositories (`lib/data/`) map the SDK's engine
/// models into these from live FFI events. No behaviour/logic beyond
/// presentation lives here.

enum DeviceKind { desktop, laptop, phone, tablet, server }

extension DeviceKindUi on DeviceKind {
  IconData get icon => switch (this) {
    DeviceKind.desktop => Icons.desktop_windows_rounded,
    DeviceKind.laptop => Icons.laptop_mac_rounded,
    DeviceKind.phone => Icons.smartphone_rounded,
    DeviceKind.tablet => Icons.tablet_mac_rounded,
    DeviceKind.server => Icons.dns_rounded,
  };
  String get label => switch (this) {
    DeviceKind.desktop => 'Desktop',
    DeviceKind.laptop => 'Laptop',
    DeviceKind.phone => 'Phone',
    DeviceKind.tablet => 'Tablet',
    DeviceKind.server => 'Server',
  };
}

/// How a device is reachable — drives capability badges and route hints.
enum Reach { lan, tailscale }

extension ReachUi on Reach {
  String get label => switch (this) {
    Reach.lan => 'LAN',
    Reach.tailscale => 'Tailscale',
  };
  IconData get icon => switch (this) {
    Reach.lan => Icons.wifi_rounded,
    Reach.tailscale => Icons.shield_rounded,
  };
}

class Device {
  final String id;
  final String name;
  final DeviceKind kind;
  final bool online;
  final Set<Reach> reach;
  final int? latencyMs;

  /// OS as the engine reports it ('linux' | 'macos' | 'windows' | 'android' |
  /// 'ios' | 'web'). Distinct from [kind], which is the form factor: a laptop
  /// and a server can both be Linux.
  final String platform;

  const Device({
    required this.id,
    required this.name,
    required this.kind,
    required this.online,
    required this.reach,
    this.latencyMs,
    this.platform = 'linux',
  });

  /// Copy with selected fields replaced.
  ///
  /// Exists so the repository's update paths cannot silently drop a field:
  /// they used to re-list every one by hand, which meant every field added to
  /// this class had to be remembered in two more places or it would vanish the
  /// first time a device went offline.
  Device copyWith({bool? online, int? latencyMs}) => Device(
    id: id,
    name: name,
    kind: kind,
    online: online ?? this.online,
    reach: reach,
    latencyMs: latencyMs ?? this.latencyMs,
    platform: platform,
  );
}

enum TransferDirection { sending, receiving }

/// Where a transfer is in its life.
///
/// [interrupted] is the odd one out and deliberately so: it is the only state
/// that outlives the process. A transfer reaches it when the link drops or the
/// app closes mid-flight, and it comes back from the engine's checkpoint after
/// a restart rather than from any event this session saw. Nothing will ever
/// emit progress for one — the only ways out are Resume, Discard, or (for an
/// inbound transfer) its sender offering it again.
enum TransferState {
  pending,
  transferring,
  paused,
  completed,
  failed,
  interrupted,
}

extension TransferStateUi on TransferState {
  String get label => switch (this) {
    TransferState.pending => 'Pending',
    TransferState.transferring => 'Transferring',
    TransferState.paused => 'Paused',
    TransferState.completed => 'Completed',
    TransferState.failed => 'Failed',
    TransferState.interrupted => 'Interrupted',
  };
}

class Transfer {
  final String id;
  final String peerName;
  final String fileName;
  final TransferDirection direction;
  final TransferState state;
  final int totalBytes;
  final int doneBytes;

  /// Current speed in bytes/second (0 when unknown/idle).
  final double speedBps;

  /// Estimated seconds remaining, or null when unknown.
  final int? etaSecs;

  /// Whether this side can restart it, for a [TransferState.interrupted] row.
  ///
  /// Only an outgoing transfer can be: the transfer protocol is sender-driven,
  /// so an interrupted receive continues when its sender offers it again and a
  /// Resume button on one would do nothing. Meaningless — and false — for
  /// every other state.
  final bool resumable;

  /// Whether the sending device was pinned by the handshake that offered this
  /// transfer — the first time these two devices have ever spoken.
  ///
  /// What makes an approval prompt say so, and what makes a refusal un-pin the
  /// peer. False for everything else, including every outgoing transfer.
  final bool newlyTrusted;

  /// The session's pairing code — a 128-bit safety number both devices derive
  /// from the keys they actually negotiated.
  ///
  /// Shown, in full, on a first-contact prompt so the user can compare it with
  /// the other device's screen. Never compared here: this device has no way to
  /// know what the other one is showing, and anything claiming otherwise would
  /// be inventing the one check the code exists to make the user perform.
  final String pairingCode;

  const Transfer({
    required this.id,
    required this.peerName,
    required this.fileName,
    required this.direction,
    required this.state,
    required this.totalBytes,
    required this.doneBytes,
    this.speedBps = 0,
    this.etaSecs,
    this.resumable = false,
    this.newlyTrusted = false,
    this.pairingCode = '',
  });

  double get progress =>
      totalBytes == 0 ? 0 : (doneBytes / totalBytes).clamp(0, 1).toDouble();

  Transfer copyWith({
    TransferState? state,
    int? doneBytes,
    double? speedBps,
    int? etaSecs,
    bool? resumable,
  }) => Transfer(
    id: id,
    peerName: peerName,
    fileName: fileName,
    direction: direction,
    state: state ?? this.state,
    totalBytes: totalBytes,
    doneBytes: doneBytes ?? this.doneBytes,
    speedBps: speedBps ?? this.speedBps,
    etaSecs: etaSecs ?? this.etaSecs,
    resumable: resumable ?? this.resumable,
    // Both are facts about the handshake that offered this transfer, settled
    // before the first progress update and true for its whole life. Nothing
    // that happens later may revise them, so they are carried, never copied
    // over — a `copyWith` that could blank the pairing code would let a
    // first-contact prompt lose the very thing it exists to show.
    newlyTrusted: newlyTrusted,
    pairingCode: pairingCode,
  );
}

class HistoryItem {
  final String id;
  final String peerName;
  final String fileName;
  final TransferDirection direction;
  final DateTime at;
  final bool success;
  final int bytes;

  /// Local path of the item; empty when unknown.
  final String path;

  const HistoryItem({
    required this.id,
    required this.peerName,
    required this.fileName,
    required this.direction,
    required this.at,
    required this.success,
    required this.bytes,
    this.path = '',
  });
}

/// Human-readable byte size.
String formatBytes(int bytes) {
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  var size = bytes.toDouble();
  var unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit++;
  }
  final rounded = unit == 0 ? size.toStringAsFixed(0) : size.toStringAsFixed(1);
  return '$rounded ${units[unit]}';
}

/// How long ago [t] was, in coarse human terms (`just now`, `5m ago`, `3d
/// ago`). Shared by History and the Conversations list so the two never
/// describe the same instant differently.
String formatAgo(DateTime t) {
  final d = DateTime.now().difference(t);
  if (d.inMinutes < 1) return 'just now';
  if (d.inMinutes < 60) return '${d.inMinutes}m ago';
  if (d.inHours < 24) return '${d.inHours}h ago';
  return '${d.inDays}d ago';
}

/// Human-readable transfer speed, e.g. `1.2 MB/s`. Empty when idle/unknown.
String formatSpeed(double bytesPerSecond) {
  if (bytesPerSecond <= 0) return '';
  return '${formatBytes(bytesPerSecond.round())}/s';
}

/// Human-readable ETA, e.g. `45s left` / `3m 20s left`. Empty when unknown.
String formatEta(int? seconds) {
  if (seconds == null || seconds < 0) return '';
  if (seconds < 60) return '${seconds}s left';
  final m = seconds ~/ 60;
  final s = seconds % 60;
  if (m < 60) return s == 0 ? '${m}m left' : '${m}m ${s}s left';
  final h = m ~/ 60;
  return '${h}h ${m % 60}m left';
}
