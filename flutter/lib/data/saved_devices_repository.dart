import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:shared_preferences/shared_preferences.dart';

/// A device the user saved by address (host/IP or MagicDNS name + port), so it
/// stays visible and tappable without live discovery — the answer for peers
/// discovery can't surface (Tailscale on Android, headless-by-IP). Purely
/// local; no cloud, no credentials.
@immutable
class SavedDevice {
  final String id;
  final String name;
  final String host;
  final int port;
  const SavedDevice({
    required this.id,
    required this.name,
    required this.host,
    required this.port,
  });

  Map<String, dynamic> toJson() => {
    'id': id,
    'name': name,
    'host': host,
    'port': port,
  };

  factory SavedDevice.fromJson(Map<String, dynamic> j) => SavedDevice(
    id: j['id'] as String,
    name: j['name'] as String,
    host: j['host'] as String,
    port: (j['port'] as num).toInt(),
  );
}

/// Persistent list of saved devices, backed by [SharedPreferences]. Loads once
/// on [load]; every mutation persists and notifies.
class SavedDevicesRepository extends ChangeNotifier {
  static const _key = 'saved_devices_v1';
  List<SavedDevice> _items = [];
  bool _disposed = false;

  List<SavedDevice> get devices => List.unmodifiable(_items);

  /// Load the saved list from disk (call once at startup).
  Future<void> load() async {
    try {
      final prefs = await SharedPreferences.getInstance();
      final raw = prefs.getString(_key);
      if (raw != null) {
        final list = jsonDecode(raw) as List;
        final parsed = <SavedDevice>[];
        for (final entry in list) {
          try {
            parsed.add(
              SavedDevice.fromJson((entry as Map).cast<String, dynamic>()),
            );
          } catch (_) {
            // Skip only the corrupt entry; keep the rest.
          }
        }
        _items = parsed;
      }
    } catch (_) {
      // Corrupt/absent store → start empty rather than crash.
    }
    if (!_disposed) notifyListeners();
  }

  /// Add a device and persist. Returns the created entry.
  Future<SavedDevice> add({
    required String name,
    required String host,
    required int port,
  }) async {
    final device = SavedDevice(
      id: '${DateTime.now().microsecondsSinceEpoch}',
      name: name,
      host: host,
      port: port,
    );
    _items = [..._items, device];
    if (!_disposed) notifyListeners();
    await _persist();
    return device;
  }

  /// Update a device's details (same id) and persist.
  Future<void> update(
    String id, {
    required String name,
    required String host,
    required int port,
  }) async {
    _items = [
      for (final d in _items)
        d.id == id
            ? SavedDevice(id: d.id, name: name, host: host, port: port)
            : d,
    ];
    if (!_disposed) notifyListeners();
    await _persist();
  }

  /// Remove a device by id and persist.
  Future<void> remove(String id) async {
    _items = _items.where((d) => d.id != id).toList();
    if (!_disposed) notifyListeners();
    await _persist();
  }

  Future<void> _persist() async {
    try {
      final prefs = await SharedPreferences.getInstance();
      await prefs.setString(
        _key,
        jsonEncode(_items.map((d) => d.toJson()).toList()),
      );
    } catch (_) {
      // Best-effort; the in-memory list stays correct for this session.
    }
  }

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
  }
}
