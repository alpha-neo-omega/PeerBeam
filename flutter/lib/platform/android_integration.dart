import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';

import '../data/history_repository.dart';
import '../data/transfer_repository.dart';
import '../state/models.dart';
import '../state/staging.dart';
import '../state/stores.dart';
import 'bridge.dart';
import 'notifications.dart';
import 'services.dart';
import 'shared_item.dart';

/// Wires the Android platform into the app: routes share/receive intents into
/// the staging store, exposes the latest shared text, and keeps the
/// foreground service in sync with active transfers + background-receive mode.
///
/// Entirely driven through a [PlatformBridge], so it runs as a harmless no-op
/// off Android and is testable with a fake bridge.
class AndroidIntegration {
  final PlatformBridge bridge;
  final StagingStore staging;
  final TransferRepository transfer;
  final SettingsStore settings;
  final HistoryRepository history;

  late final ForegroundServiceController service = ForegroundServiceController(
    bridge,
  );
  late final BatteryOptimization battery = BatteryOptimization(bridge);

  /// Latest text handed to us via a share intent (e.g. to send as clipboard).
  final ValueNotifier<String?> sharedText = ValueNotifier<String?>(null);

  /// Fires after shared files land in staging, so the UI can surface the
  /// staged sheet.
  Stream<void> get filesShared => _filesShared.stream;
  final StreamController<void> _filesShared = StreamController.broadcast();

  StreamSubscription<Map<String, dynamic>>? _sub;

  /// History ids already seen, so we only notify about *new* completions —
  /// never the pre-existing history on cold start. `null` until the first
  /// `_onHistoryChanged` pass has established a baseline.
  Set<String>? _seenHistoryIds;

  AndroidIntegration({
    required this.bridge,
    required this.staging,
    required this.transfer,
    required this.settings,
    required this.history,
  });

  Future<void> start() async {
    // Fire-and-forget: no-op off Android / pre-13, and we don't need to await
    // the grant — a denial just means notifications keep silently no-oping.
    unawaited(bridge.requestNotificationPermission());

    _sub = bridge.events().listen(_onEvent);
    final initial = await bridge.initialIntent();
    if (initial != null) _onEvent(initial);
    transfer.addListener(_onStoreChanged);
    settings.addListener(_onStoreChanged);
    history.addListener(_onHistoryChanged);
    _onHistoryChanged(); // seed the baseline before reacting to changes
    await _syncService();
  }

  void _onEvent(Map<String, dynamic> event) {
    final items = parseSharedEvent(event);
    final files = <StagedFile>[];
    for (final item in items) {
      switch (item.kind) {
        case SharedKind.file:
          files.add(
            StagedFile(
              path: item.path!,
              name: item.name ?? item.path!,
              size: _sizeOf(item.path!), // intents don't carry a size
            ),
          );
        case SharedKind.text:
          sharedText.value = item.text;
      }
    }
    if (files.isNotEmpty && staging.add(files) > 0) {
      _filesShared.add(null);
    }
  }

  static int _sizeOf(String path) {
    try {
      return File(path).lengthSync();
    } catch (_) {
      return 0;
    }
  }

  void _onStoreChanged() => unawaited(_syncService());

  Future<void> _syncService() => service.sync(
    activeTransfers: transfer.activeCount,
    receiving: settings.backgroundReceive,
    incoming: _hasActiveReceive(),
  );

  /// Whether any transfer currently occupying the foreground-service
  /// notification is a receive — selects the download icon over the upload
  /// one while the service is showing an active-transfer notification.
  bool _hasActiveReceive() => transfer.transfers.any(
    (t) =>
        t.direction == TransferDirection.receiving &&
        (t.state == TransferState.transferring ||
            t.state == TransferState.pending ||
            t.state == TransferState.paused),
  );

  /// A backgrounded *sender* gets no other feedback once the transfers screen
  /// isn't visible, so post a notification for newly-settled sends. History
  /// only ever gains entries for `transfer_completed`/`transfer_failed` (a
  /// cancelled transfer is never recorded), so a history id we haven't seen
  /// before unambiguously means "this send just finished" — no fragile
  /// diffing of the (already-removed-by-then) active transfer list needed.
  /// Received files are handled separately in `main.dart` (step 2), so this
  /// only reacts to the sending direction to avoid double-notifying.
  void _onHistoryChanged() {
    final items = history.items;
    final ids = items.map((i) => i.id).toSet();
    final seen = _seenHistoryIds;
    if (seen != null) {
      for (final item in items) {
        if (!seen.contains(item.id) &&
            item.direction == TransferDirection.sending) {
          _notifySendResult(item);
        }
      }
    }
    _seenHistoryIds = ids;
  }

  void _notifySendResult(HistoryItem item) {
    if (!settings.notifications) return;
    final id = TransferNotifications.idFor(item.id);
    final content = item.success
        ? TransferNotifications.complete(
            notificationId: id,
            fileName: item.fileName,
            sending: true,
          )
        : TransferNotifications.failed(
            notificationId: id,
            fileName: item.fileName,
          );
    unawaited(bridge.showNotification(content));
  }

  void dispose() {
    _sub?.cancel();
    _filesShared.close();
    transfer.removeListener(_onStoreChanged);
    settings.removeListener(_onStoreChanged);
    history.removeListener(_onHistoryChanged);
    sharedText.dispose();
  }
}
