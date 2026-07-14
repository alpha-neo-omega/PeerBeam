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
@immutable
class PeerTarget {
  final String name;
  final List<String> addresses;
  final int port;
  const PeerTarget({
    required this.name,
    required this.addresses,
    required this.port,
  });

  Map<String, dynamic> toJson() => {
    'name': name,
    'addresses': addresses,
    'port': port,
  };
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
