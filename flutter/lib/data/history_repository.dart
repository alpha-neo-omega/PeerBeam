// ignore_for_file: prefer_initializing_formals
import 'dart:async';

import 'package:flutter/foundation.dart';

import '../sdk/events.dart';
import '../sdk/models.dart';
import '../sdk/peerbeam.dart';
import '../state/models.dart';

/// Completed-transfer history. Refetched from the engine whenever it signals
/// `history_updated` (no polling). Same UI surface (`items`, `clear`).
class HistoryRepository extends ChangeNotifier {
  final PeerBeamApi? _api;
  List<HistoryItem> _items = [];
  StreamSubscription<BridgeEvent>? _sub;
  bool _disposed = false;

  HistoryRepository({PeerBeamApi? api}) : _api = api {
    _sub = _api?.events.listen((e) {
      if (e is HistoryUpdated) refresh();
    });
    if (_api != null) refresh();
  }

  List<HistoryItem> get items => List.unmodifiable(_items);

  /// Pull the latest history from the engine.
  Future<void> refresh() async {
    final api = _api;
    if (api == null) return;
    try {
      final entries = await api.history();
      if (_disposed) return; // disposed while the fetch was in flight
      _items = entries.map(_map).toList().reversed.toList();
      notifyListeners();
    } catch (_) {
      // Leave the current view on transient errors.
    }
  }

  /// Local view clear. (An engine-backed clear lands with the M3 history ops.)
  void clear() {
    if (_items.isEmpty) return;
    _items = [];
    notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _sub?.cancel();
    super.dispose();
  }

  static HistoryItem _map(HistoryEntry e) => HistoryItem(
    id: e.id,
    peerName: e.peer,
    fileName: e.file,
    direction: e.direction == 'receiving'
        ? TransferDirection.receiving
        : TransferDirection.sending,
    at: DateTime.tryParse(e.at) ?? DateTime.now(),
    success: e.success,
    bytes: e.bytes,
  );
}
