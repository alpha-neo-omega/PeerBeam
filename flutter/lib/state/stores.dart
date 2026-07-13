import 'package:flutter/material.dart';

import '../data/discovery_repository.dart';
import '../data/history_repository.dart';
import '../data/transfer_repository.dart';
import '../sdk/peerbeam.dart';
import 'models.dart';
import 'staging.dart';

/// Per-domain state. Screens listen to only the piece they need (via
/// `AnimatedBuilder`), so a change in one domain never rebuilds the whole app.
///
/// Device/transfer/history state now lives in **repositories** that are driven
/// by engine events (see `lib/data/`); the classes below are the remaining
/// UI-local pieces (theme, settings, staging).

class ThemeController extends ChangeNotifier {
  ThemeMode _mode = ThemeMode.system;
  ThemeMode get mode => _mode;
  void setMode(ThemeMode mode) {
    if (mode == _mode) return;
    _mode = mode;
    notifyListeners();
  }
}

class SettingsStore extends ChangeNotifier {
  String deviceName;
  String saveDirectory;
  bool autoAcceptTrusted;
  bool notifications;
  bool compression;

  /// Keep a foreground service running to receive files while backgrounded.
  bool backgroundReceive;

  SettingsStore({
    required this.deviceName,
    required this.saveDirectory,
    required this.autoAcceptTrusted,
    required this.notifications,
    required this.compression,
    this.backgroundReceive = false,
  });

  void setBackgroundReceive(bool v) {
    backgroundReceive = v;
    notifyListeners();
  }

  void setDeviceName(String v) {
    deviceName = v;
    notifyListeners();
  }

  void setSaveDirectory(String v) {
    saveDirectory = v;
    notifyListeners();
  }

  void setAutoAccept(bool v) {
    autoAcceptTrusted = v;
    notifyListeners();
  }

  void setNotifications(bool v) {
    notifications = v;
    notifyListeners();
  }

  void setCompression(bool v) {
    compression = v;
    notifyListeners();
  }
}

/// Top-level container of all state, created once and shared via [AppScope].
class AppState {
  final ThemeController theme;
  final DiscoveryRepository device;
  final TransferRepository transfer;
  final HistoryRepository history;
  final SettingsStore settings;
  final StagingStore staging;

  AppState({
    required this.theme,
    required this.device,
    required this.transfer,
    required this.history,
    required this.settings,
    required this.staging,
  });

  /// Production wiring: repositories driven by the live engine over [api].
  factory AppState.live(PeerBeamApi api) {
    return AppState(
      theme: ThemeController(),
      device: DiscoveryRepository(api: api),
      transfer: TransferRepository(api: api),
      history: HistoryRepository(api: api),
      settings: SettingsStore(
        deviceName: 'This Device',
        saveDirectory: '~/Downloads/PeerBeam',
        autoAcceptTrusted: false,
        notifications: true,
        compression: true,
      ),
      staging: StagingStore(),
    );
  }

  void dispose() {
    theme.dispose();
    device.dispose();
    transfer.dispose();
    history.dispose();
    settings.dispose();
    staging.dispose();
  }

  /// Sample data so the modern UI is fully explorable without the engine.
  factory AppState.sample() {
    final now = DateTime.now();
    return AppState(
      theme: ThemeController(),
      device: DiscoveryRepository(seed: [
        const Device(
          id: 'd1',
          name: "Alice's MacBook",
          kind: DeviceKind.laptop,
          online: true,
          reach: {Reach.lan, Reach.tailscale},
          latencyMs: 4,
        ),
        const Device(
          id: 'd2',
          name: 'Pixel 9',
          kind: DeviceKind.phone,
          online: true,
          reach: {Reach.lan},
          latencyMs: 11,
        ),
        const Device(
          id: 'd3',
          name: 'home-server',
          kind: DeviceKind.server,
          online: true,
          reach: {Reach.tailscale},
          latencyMs: 38,
        ),
        const Device(
          id: 'd4',
          name: 'Studio PC',
          kind: DeviceKind.desktop,
          online: false,
          reach: {Reach.lan},
        ),
      ]),
      transfer: TransferRepository(seed: [
        const Transfer(
          id: 't1',
          peerName: "Alice's MacBook",
          fileName: 'holiday-photos.zip',
          direction: TransferDirection.sending,
          state: TransferState.transferring,
          totalBytes: 512 * 1024 * 1024,
          doneBytes: 331 * 1024 * 1024,
        ),
        const Transfer(
          id: 't2',
          peerName: 'Pixel 9',
          fileName: 'presentation.pdf',
          direction: TransferDirection.receiving,
          state: TransferState.paused,
          totalBytes: 24 * 1024 * 1024,
          doneBytes: 8 * 1024 * 1024,
        ),
      ]),
      history: HistoryRepository(seed: [
        HistoryItem(
          id: 'h1',
          peerName: "Alice's MacBook",
          fileName: 'design-review.sketch',
          direction: TransferDirection.receiving,
          at: now.subtract(const Duration(minutes: 12)),
          success: true,
          bytes: 84 * 1024 * 1024,
        ),
        HistoryItem(
          id: 'h2',
          peerName: 'home-server',
          fileName: 'backup-2026.tar.zst',
          direction: TransferDirection.sending,
          at: now.subtract(const Duration(hours: 3)),
          success: true,
          bytes: 3 * 1024 * 1024 * 1024,
        ),
        HistoryItem(
          id: 'h3',
          peerName: 'Pixel 9',
          fileName: 'video.mov',
          direction: TransferDirection.sending,
          at: now.subtract(const Duration(days: 1)),
          success: false,
          bytes: 1200 * 1024 * 1024,
        ),
      ]),
      settings: SettingsStore(
        deviceName: 'This Device',
        saveDirectory: '~/Downloads/PeerBeam',
        autoAcceptTrusted: false,
        notifications: true,
        compression: true,
      ),
      staging: StagingStore(),
    );
  }
}
