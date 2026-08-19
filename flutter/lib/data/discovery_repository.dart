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
  bool _disposed = false;

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
    return PeerTarget(
      id: d.id,
      name: d.name,
      addresses: d.addresses,
      port: d.port,
    );
  }

  /// The discovered device currently advertising [host] on [port], if any.
  ///
  /// Lets a saved (by-address) entry be resolved to the peer's **real** device
  /// id, which is the only id the engine keys a conversation by. A saved
  /// entry's own id is locally minted and means nothing to the peer, so
  /// anything that needs a genuine identity has to come through here — and
  /// accept a null when discovery cannot currently see the peer.
  ///
  /// Matched on the exact advertised address: a saved MagicDNS/host name that
  /// discovery reports as an IP will not match, which is the honest answer
  /// (nothing here can prove the two are the same machine).
  Device? deviceAtAddress(String host, int port) {
    for (final raw in _raw.values) {
      if (raw.port == port && raw.addresses.contains(host)) {
        return _byId[raw.id];
      }
    }
    return null;
  }

  /// Start discovery and reflect it in [scanning] (used at boot, so the
  /// Scan/Stop control is truthful from the first frame). Safe to call when
  /// already scanning.
  Future<void> start() async {
    if (_scanning) return;
    _scanning = true;
    notifyListeners();
    try {
      await _api?.startDiscovery();
    } catch (_) {
      if (_disposed) return;
      _scanning = false;
      notifyListeners();
    }
  }

  /// Start/stop discovery in the engine; UI state flips optimistically.
  void toggleScan() {
    final previous = _scanning;
    _scanning = !_scanning;
    notifyListeners();
    final fut = _scanning ? _api?.startDiscovery() : _api?.stopDiscovery();
    fut?.catchError((_) {
      if (_disposed) return; // disposed before the toggle resolved
      // Revert to the state captured before this toggle — restoring by
      // negating the *current* flag is wrong under rapid toggle+failure,
      // since a later toggle may have already changed it again.
      _scanning = previous;
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
      case DeviceResync():
        unawaited(_resync());
        return;
      default:
        return;
    }
    notifyListeners();
  }

  /// Re-pull the authoritative device list after a [DeviceResync] hint (the
  /// native event stream lagged and silently dropped device transitions).
  /// Rebuilds `_byId`/`_raw` from scratch so ghost devices are dropped and
  /// any missed additions reappear.
  Future<void> _resync() async {
    final api = _api;
    if (api == null) return;
    try {
      final list = await api.devices();
      if (_disposed) return;
      final freshIds = list.map((d) => d.id).toSet();
      _byId.removeWhere((id, _) => !freshIds.contains(id));
      _raw.removeWhere((id, _) => !freshIds.contains(id));
      for (final d in list) {
        _raw[d.id] = d;
        _byId[d.id] = _map(d);
      }
      notifyListeners();
    } catch (_) {
      // Best-effort recovery; the next event or resync will retry.
    }
  }

  @override
  void dispose() {
    _disposed = true;
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
    platform: d.platform,
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

  static Device _withOnline(Device d, bool online) =>
      d.copyWith(online: online);

  /// An explicit null clears the reading rather than keeping the last one —
  /// the engine sends one when it holds a live link it cannot characterise,
  /// and continuing to show the old number would present it as current.
  static Device _withLatency(Device d, int? latencyMs) =>
      d.copyWith(latencyMs: latencyMs);

  /// Ask [peer] to make itself findable, returning whether the request went
  /// out.
  ///
  /// Whether the device rings is its own decision and it never answers, so a
  /// `true` here means "asked", never "rang".
  Future<bool> ring(PeerTarget peer, {int seconds = 15}) async {
    final api = _api;
    if (api == null) return false;
    try {
      return await api.presenceRing(peer, seconds: seconds);
    } catch (_) {
      return false;
    }
  }
}
