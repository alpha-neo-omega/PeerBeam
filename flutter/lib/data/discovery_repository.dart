// ignore_for_file: prefer_initializing_formals
import 'dart:async';

import 'package:flutter/foundation.dart';

import '../sdk/events.dart';
import '../sdk/models.dart';
import '../sdk/peerbeam.dart';
import '../state/models.dart';

/// Reactive device list, driven entirely by engine events (never polls). Keeps
/// the same surface the UI already used (`devices`, `scanning`, `toggleScan`,
/// `onlineCount`) so no widget changes — the data source is now the engine.
class DiscoveryRepository extends ChangeNotifier {
  final PeerBeamApi? _api;
  final Map<String, Device> _byId = {};
  // Keep the raw SDK device (addresses + port) so a send can target it.
  final Map<String, SdkDevice> _raw = {};
  bool _scanning = false;
  StreamSubscription<BridgeEvent>? _sub;

  DiscoveryRepository({PeerBeamApi? api}) : _api = api {
    _sub = _api?.events.listen(_onEvent);
  }

  List<Device> get devices => List.unmodifiable(_byId.values);
  bool get scanning => _scanning;
  int get onlineCount => _byId.values.where((d) => d.online).length;

  /// A send target for a discovered device, or null if unknown/unaddressed.
  PeerTarget? peerTarget(String id) {
    final d = _raw[id];
    if (d == null || d.addresses.isEmpty || d.port == 0) return null;
    return PeerTarget(name: d.name, addresses: d.addresses, port: d.port);
  }

  /// Start/stop discovery in the engine; UI state flips optimistically.
  void toggleScan() {
    _scanning = !_scanning;
    notifyListeners();
    final fut = _scanning ? _api?.startDiscovery() : _api?.stopDiscovery();
    fut?.catchError((_) {
      // Revert on failure; keep the UI honest.
      _scanning = !_scanning;
      notifyListeners();
    });
  }

  void _onEvent(BridgeEvent e) {
    switch (e) {
      case DeviceAdded(:final device):
      case DeviceUpdated(:final device):
        _raw[device.id] = device;
        _byId[device.id] = _map(device);
      case DeviceRemoved(:final id):
        _byId.remove(id);
        _raw.remove(id);
      case DeviceStatusChanged(:final id, :final online):
        final d = _byId[id];
        if (d != null) _byId[id] = _withOnline(d, online);
      case DeviceLatencyChanged(:final id, :final latencyMs):
        final d = _byId[id];
        if (d != null) _byId[id] = _withLatency(d, latencyMs);
      default:
        return;
    }
    notifyListeners();
  }

  @override
  void dispose() {
    _sub?.cancel();
    super.dispose();
  }

  // ── SDK → UI model ──────────────────────────────────────────
  static Device _map(SdkDevice d) => Device(
    id: d.id,
    name: d.name,
    kind: _kind(d.kind),
    online: d.online,
    reach: _reach(d),
    latencyMs: d.latencyMs,
  );

  static DeviceKind _kind(String k) => switch (k) {
    'laptop' => DeviceKind.laptop,
    'phone' => DeviceKind.phone,
    'tablet' => DeviceKind.tablet,
    'server' => DeviceKind.server,
    _ => DeviceKind.desktop,
  };

  static Set<Reach> _reach(SdkDevice d) {
    final r = <Reach>{};
    if (d.reachableLan) r.add(Reach.lan);
    if (d.reachableRemote) r.add(Reach.tailscale);
    if (r.isEmpty) r.add(Reach.lan);
    return r;
  }

  static Device _withOnline(Device d, bool online) => Device(
    id: d.id,
    name: d.name,
    kind: d.kind,
    online: online,
    reach: d.reach,
    latencyMs: d.latencyMs,
  );

  static Device _withLatency(Device d, int? latencyMs) => Device(
    id: d.id,
    name: d.name,
    kind: d.kind,
    online: d.online,
    reach: d.reach,
    latencyMs: latencyMs,
  );
}
