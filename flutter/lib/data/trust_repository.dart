// ignore_for_file: prefer_initializing_formals
import 'dart:async';

import 'package:flutter/foundation.dart';

import '../sdk/error_text.dart';

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
  ///
  /// Returns null when it worked, or a message when it did not. **The caller
  /// has to say so**: this used to swallow the failure and return nothing, so a
  /// revoke that never happened looked exactly like one that did — the dialog
  /// closed, the row stayed, and the user was left believing they had withdrawn
  /// trust from a device that still holds it. That is the one failure on this
  /// screen that must never be quiet.
  Future<String?> remove(String id) async {
    try {
      await _api?.trustRemove(id);
      return null;
    } catch (e) {
      return friendlyError(e);
    }
  }

  /// Approve a pinned device directly, without waiting for a transfer from it.
  ///
  /// Returns null on success, or a sentence to show the user. Two distinct
  /// failures, and they must not read alike:
  ///
  /// * The engine refused or could not be reached — reported as itself.
  /// * The device is **not pinned**. The engine answers this without erroring,
  ///   because it is not an error: approval refines a key remembered from a
  ///   handshake, and a device this machine has never spoken to has presented
  ///   none. Saying "approved" there would show a device as trusted while the
  ///   store held nothing for it.
  ///
  /// Not optimistic, unlike [setPermission]. Approval is a standing, not a
  /// toggle: showing it as granted before the engine agreed would be the one
  /// guess on this screen worth nothing if wrong. The `trust_changed` event
  /// drives the refresh.
  Future<String?> approve(String id, {bool share = true}) async {
    final api = _api;
    if (api == null) return 'The engine is not running.';
    try {
      final pinned = await api.trustApprove(id, share: share);
      if (!pinned) {
        return 'This device has never connected, so there is no key to '
            'vouch for. It can be approved once it has.';
      }
      await refresh();
      return null;
    } catch (e) {
      return friendlyError(e);
    }
  }

  /// Stop asking about a device's files, or start asking again.
  ///
  /// Returns null on success or a sentence to show. Not optimistic: unlike a
  /// permission switch, being wrong here means the user believes they will be
  /// asked about a file and are not — or the reverse. The `trust_changed` event
  /// drives the refresh.
  Future<String?> setAutoAccept(String id, bool autoAccept) async {
    final api = _api;
    if (api == null) return 'The engine is not running.';
    try {
      await api.trustSetAutoAccept(id, autoAccept);
      await refresh();
      return null;
    } catch (e) {
      return friendlyError(e);
    }
  }

  /// The trusted record for [id], or null when this device is not pinned.
  TrustedDevice? byId(String id) {
    for (final d in _items) {
      if (d.id == id) return d;
    }
    return null;
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
            // Carried, not defaulted. This guess is about **one permission**;
            // rebuilding the record without this field would silently show
            // auto-accept as off until the next refresh, and a user who then
            // tapped it would be turning on something that was already on.
            autoAccept: d.autoAccept,
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
