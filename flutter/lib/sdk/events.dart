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
  String? get peer => payload['peer'] as String?;
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
