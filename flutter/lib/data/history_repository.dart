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

  /// Note: does NOT refresh in the constructor. Repositories are constructed
  /// synchronously in `AppState.live` during `initState`, before the engine's
  /// `initialize()` has been awaited — an early `refresh()` would just hit
  /// `not_initialised` and be swallowed, leaving history looking empty until
  /// the next `history_updated` event. Callers must explicitly `refresh()`
  /// once the engine is initialized (see the boot sequence in `main.dart`).
  HistoryRepository({PeerBeamApi? api}) : _api = api {
    _sub = _api?.events.listen((e) {
      if (e is HistoryUpdated) refresh();
    });
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

  /// Clear history in the engine (persisted); the local view empties
  /// immediately and the engine's history_updated confirms.
  void clear() {
    unawaited(_api?.historyClear().catchError((_) {}));
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
    path: e.path,
  );
}
