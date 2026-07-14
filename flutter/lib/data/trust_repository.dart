// ignore_for_file: prefer_initializing_formals
import 'dart:async';

import 'package:flutter/foundation.dart';

import '../sdk/events.dart';
import '../sdk/models.dart';
import '../sdk/peerbeam.dart';

/// Pinned (trusted) devices, refetched from the engine whenever trust changes
/// (a revoke here, or a new pin after an accepted transfer).
class TrustRepository extends ChangeNotifier {
  final PeerBeamApi? _api;
  List<TrustedDevice> _items = [];
  StreamSubscription<BridgeEvent>? _sub;
  bool _disposed = false;

  TrustRepository({PeerBeamApi? api}) : _api = api {
    _sub = _api?.events.listen((e) {
      // New pins land during transfers (TOFU on accept), so a history change
      // is also a trust-refresh signal.
      if (e is TrustChanged || e is HistoryUpdated) refresh();
    });
    if (_api != null) refresh();
  }

  List<TrustedDevice> get items => List.unmodifiable(_items);

  /// Pull the latest pins from the engine.
  Future<void> refresh() async {
    final api = _api;
    if (api == null) return;
    try {
      final devices = await api.trustList();
      if (_disposed) return;
      _items = devices;
      notifyListeners();
    } catch (_) {
      // Keep the current view on transient errors.
    }
  }

  /// Revoke a pin; the engine emits `trust_changed`, which refreshes us.
  Future<void> remove(String id) async {
    try {
      await _api?.trustRemove(id);
    } catch (_) {}
  }

  @override
  void dispose() {
    _disposed = true;
    _sub?.cancel();
    super.dispose();
  }
}
