// ignore_for_file: prefer_initializing_formals
import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:shared_preferences/shared_preferences.dart';

import '../sdk/events.dart';
import '../sdk/exceptions.dart';
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

  /// Provider-scoped id → the authenticated device id that answered there.
  ///
  /// A Tailscale peer is discovered as `ts:<node>`, which is Tailscale's name
  /// for it and not one a conversation can be filed under. [identify] dials and
  /// learns the real one, and **this is where that answer survives the tap**.
  ///
  /// Without it the thread was openable exactly once. It is filed under the
  /// answered id, so reopening it from Conversations asks [peerTarget] for an
  /// id discovery has never reported — `_raw` is keyed by what the provider
  /// said — which came back null, and a null target is a disabled composer.
  /// The conversation became permanently read-only the moment you left it, and
  /// said the peer was unreachable while it sat online in the device list.
  ///
  /// It maps id to **id**, never id to address: the address always comes from
  /// whatever discovery is reporting right now, so a node that moves cannot
  /// leave a stale route behind. A tailnet node id is stable, which is what
  /// makes it worth persisting at all.
  final Map<String, String> _identified = {};

  /// Where [_identified] is kept between runs. Versioned, because the shape of
  /// what an id means is exactly the kind of thing that changes.
  static const _identifiedKey = 'resolved_identities_v1';

  DiscoveryRepository({PeerBeamApi? api}) : _api = api {
    _sub = _api?.events.listen(_onEvent);
  }

  /// Read the remembered identities. Call once at boot; safe to call again.
  Future<void> loadIdentities() async {
    try {
      final prefs = await SharedPreferences.getInstance();
      final raw = prefs.getString(_identifiedKey);
      if (raw == null) return;
      final decoded = jsonDecode(raw);
      if (decoded is! Map) return;
      for (final entry in decoded.entries) {
        final key = entry.key;
        final value = entry.value;
        if (key is String &&
            value is String &&
            key.isNotEmpty &&
            value.isNotEmpty) {
          _identified[key] = value;
        }
      }
      if (_identified.isNotEmpty && !_disposed) notifyListeners();
    } catch (_) {
      // A store that will not read is a conversation that needs one more dial,
      // not a boot failure.
    }
  }

  /// The authenticated id already known for [providerId], or null.
  ///
  /// Lets a surface skip the dial entirely for a peer it has identified before
  /// — including while that peer is offline, where the dial could not succeed
  /// and the thread is still worth reading.
  String? resolvedIdFor(String providerId) => _identified[providerId];

  Future<void> _rememberIdentity(String providerId, String deviceId) async {
    if (_identified[providerId] == deviceId) return;
    _identified[providerId] = deviceId;
    if (!_disposed) notifyListeners();
    try {
      final prefs = await SharedPreferences.getInstance();
      await prefs.setString(_identifiedKey, jsonEncode(_identified));
    } catch (_) {
      // Kept for this session even if it could not be written.
    }
  }

  List<Device> get devices => List.unmodifiable(_byId.values);

  /// Ask an address which device answers there, or null when it cannot be
  /// reached.
  ///
  /// Straight through to the engine — nothing is cached. A provider-scoped id
  /// (Tailscale's `ts:<node>`) or a typed address cannot carry a conversation,
  /// and the authenticated id is the only thing that can; the engine learns it
  /// by dialling and completing the ordinary handshake. Nothing is sent by
  /// asking, and nothing is approved.
  /// Returns the identity **and** why it failed, never one without the other.
  ///
  /// This used to return a bare nullable and swallow the exception, so every
  /// failure reached the user as the same sentence — "could not reach it" —
  /// whether the peer was unreachable, refused the connection, withheld a
  /// permission, or the engine was not running at all. The CLI has always said
  /// which (`could not reach host:port: <reason>`); the GUI blamed the network
  /// for all four. The error is an ordinary answer here, not something to
  /// throw into a build, but it is an answer the caller has to be *given*.
  Future<({PeerIdentity? identity, Object? error})> identify(
    PeerTarget peer,
  ) async {
    final api = _api;
    if (api == null) {
      return (identity: null, error: const PeerBeamUnavailable('no engine'));
    }
    try {
      final identity = await api.peerIdentify(peer);
      // Remember which discovered device this answer belongs to, so the
      // conversation can find its peer again after the screen is closed.
      // A target with no id of its own — a typed address — has nothing to key
      // the memory by, and is dialled afresh each time.
      final providerId = peer.id;
      if (providerId != null && providerId.isNotEmpty) {
        await _rememberIdentity(providerId, identity.deviceId);
      }
      return (identity: identity, error: null);
    } catch (e) {
      return (identity: null, error: e);
    }
  }

  bool get scanning => _scanning;
  int get onlineCount => _byId.values.where((d) => d.online).length;

  /// A send target for [id], or null when nothing addressable answers to it.
  ///
  /// [id] is looked up as a discovered id first and, failing that, as an
  /// **authenticated** one that [identify] has previously tied to a discovered
  /// device — see [_identified]. That second lookup is what lets a conversation
  /// with a Tailscale peer be reopened: the thread knows the peer only by the
  /// id that answered, and discovery knows it only as `ts:<node>`.
  ///
  /// The returned target carries the id it was **asked** for, not the one the
  /// route was found under. That matters: the engine files a conversation by
  /// `peer.id`, so handing back the `ts:` id here would write the thread into a
  /// namespace its own store refuses.
  PeerTarget? peerTarget(String id) {
    final direct = _raw[id];
    if (direct != null) return _targetFor(direct, id);
    for (final entry in _identified.entries) {
      if (entry.value != id) continue;
      final raw = _raw[entry.key];
      if (raw != null) return _targetFor(raw, id);
    }
    return null;
  }

  PeerTarget? _targetFor(SdkDevice d, String id) {
    if (d.addresses.isEmpty || d.port == 0) return null;
    return PeerTarget(
      id: id,
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
