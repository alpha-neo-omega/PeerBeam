// ignore_for_file: prefer_initializing_formals
import 'dart:async';

import 'package:flutter/foundation.dart';

import '../sdk/events.dart';
import '../sdk/models.dart';
import '../sdk/peerbeam.dart';

/// Live device status shared by trusted peers.
///
/// Event-reduced, not polled: each `presence_updated` replaces that peer's
/// entry in place, so a dashboard re-renders as heartbeats land rather than on
/// a timer of its own.
///
/// **Nothing is cached across runs.** Presence is live state — the engine
/// persists none of it, and neither does this. A cold start shows "status not
/// shared" until the first heartbeat arrives, which is the honest answer;
/// showing yesterday's battery level as current would not be.
class PresenceRepository extends ChangeNotifier {
  final PeerBeamApi? _api;
  final Map<String, SdkPresence> _byId = {};
  SdkPresence? _self;
  bool _sharing = false;
  StreamSubscription<BridgeEvent>? _sub;
  bool _disposed = false;

  /// Like `TrustRepository`, this deliberately does NOT fetch in its
  /// constructor: repositories are built synchronously in `AppState.live`
  /// before `initialize()` has been awaited, so an early call would only hit
  /// `not_initialised` and be swallowed. The boot sequence calls `refresh()`.
  PresenceRepository({PeerBeamApi? api}) : _api = api {
    _sub = _api?.events.listen((e) {
      switch (e) {
        case PresenceUpdated(:final presence):
          _byId[presence.deviceId] = presence;
          if (!_disposed) notifyListeners();
        // A revoked device must leave the dashboard at once. The engine has
        // already dropped it; refetch rather than guess which one went.
        case TrustChanged():
          unawaited(refresh());
        default:
          return;
      }
    });
  }

  /// Whether **this** device is sharing its status. Default off.
  bool get sharing => _sharing;

  /// What this device would share — a local preview for the settings copy.
  SdkPresence? get self => _self;

  /// This peer's shared status, or null when it has shared nothing. Callers
  /// must render null as "status not shared", never as zeroed gauges.
  SdkPresence? of(String deviceId) => _byId[deviceId];

  /// How many peers have shared a status.
  int get sharedCount => _byId.length;

  /// Pull the current snapshot from the engine.
  Future<void> refresh() async {
    final api = _api;
    if (api == null) return;
    try {
      final snap = await api.presence();
      if (_disposed) return;
      _sharing = snap.sharing;
      _self = snap.self;
      _byId
        ..clear()
        ..addAll(snap.devices);
      notifyListeners();
    } catch (_) {
      // Keep the current view on transient errors — a failed refresh must not
      // blank a dashboard that is still accurate.
    }
  }

  @override
  void dispose() {
    _disposed = true;
    _sub?.cancel();
    super.dispose();
  }
}
