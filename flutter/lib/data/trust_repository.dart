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

  /// Note: does NOT refresh in the constructor. Repositories are constructed
  /// synchronously in `AppState.live` during `initState`, before the engine's
  /// `initialize()` has been awaited — an early `refresh()` would just hit
  /// `not_initialised` and be swallowed, leaving trust looking empty until the
  /// next `trust_changed`/`history_updated` event. Callers must explicitly
  /// `refresh()` once the engine is initialized (see the boot sequence in
  /// `main.dart`).
  TrustRepository({PeerBeamApi? api}) : _api = api {
    _sub = _api?.events.listen((e) {
      // New pins land during transfers (TOFU on accept), so a history change
      // is also a trust-refresh signal.
      if (e is TrustChanged || e is HistoryUpdated) refresh();
    });
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

  /// Grant or withhold one permission for a device.
  ///
  /// Optimistic: the row is updated locally and listeners notified before the
  /// engine's `trust_changed` arrives, so a switch does not visibly lag the
  /// tap. The refresh that event triggers is the authority — a failed call
  /// leaves the local guess in place only until the next `refresh()`, which is
  /// why the fallback below re-reads rather than trusting the guess.
  Future<void> setPermission(
    String id,
    String permission, {
    required bool granted,
  }) async {
    final api = _api;
    if (api == null) return;
    _applyLocally(id, permission, granted);
    try {
      await api.trustSetPermission(id, permission, granted);
    } catch (_) {
      // The engine refused (an unknown permission, an unreadable store). Put
      // the truth back rather than leaving a switch showing something that did
      // not happen.
      await refresh();
    }
  }

  void _applyLocally(String id, String permission, bool granted) {
    _items = [
      for (final d in _items)
        if (d.id != id)
          d
        else
          TrustedDevice(
            id: d.id,
            name: d.name,
            fingerprint: d.fingerprint,
            trustedAt: d.trustedAt,
            approved: d.approved,
            permissions: {
              ...d.permissions.where((p) => granted || p != permission),
              if (granted) permission,
            },
          ),
    ];
    notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _sub?.cancel();
    super.dispose();
  }
}
