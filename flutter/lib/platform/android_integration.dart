import 'dart:async';

import 'package:flutter/foundation.dart';

import '../data/transfer_repository.dart';
import '../state/staging.dart';
import '../state/stores.dart';
import 'bridge.dart';
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

  late final ForegroundServiceController service = ForegroundServiceController(
    bridge,
  );
  late final BatteryOptimization battery = BatteryOptimization(bridge);

  /// Latest text handed to us via a share intent (e.g. to send as clipboard).
  final ValueNotifier<String?> sharedText = ValueNotifier<String?>(null);

  StreamSubscription<Map<String, dynamic>>? _sub;

  AndroidIntegration({
    required this.bridge,
    required this.staging,
    required this.transfer,
    required this.settings,
  });

  Future<void> start() async {
    _sub = bridge.events().listen(_onEvent);
    final initial = await bridge.initialIntent();
    if (initial != null) _onEvent(initial);
    transfer.addListener(_onStoreChanged);
    settings.addListener(_onStoreChanged);
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
              size: 0, // size resolved lazily; intents don't carry it
            ),
          );
        case SharedKind.text:
          sharedText.value = item.text;
      }
    }
    if (files.isNotEmpty) staging.add(files);
  }

  void _onStoreChanged() => unawaited(_syncService());

  Future<void> _syncService() => service.sync(
    activeTransfers: transfer.activeCount,
    receiving: settings.backgroundReceive,
  );

  void dispose() {
    _sub?.cancel();
    transfer.removeListener(_onStoreChanged);
    settings.removeListener(_onStoreChanged);
    sharedText.dispose();
  }
}
