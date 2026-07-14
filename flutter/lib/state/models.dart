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

  const Device({
    required this.id,
    required this.name,
    required this.kind,
    required this.online,
    required this.reach,
    this.latencyMs,
  });
}

enum TransferDirection { sending, receiving }

enum TransferState { pending, transferring, paused, completed, failed }

extension TransferStateUi on TransferState {
  String get label => switch (this) {
    TransferState.pending => 'Pending',
    TransferState.transferring => 'Transferring',
    TransferState.paused => 'Paused',
    TransferState.completed => 'Completed',
    TransferState.failed => 'Failed',
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
  });

  double get progress => totalBytes == 0 ? 0 : doneBytes / totalBytes;

  Transfer copyWith({
    TransferState? state,
    int? doneBytes,
    double? speedBps,
    int? etaSecs,
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
