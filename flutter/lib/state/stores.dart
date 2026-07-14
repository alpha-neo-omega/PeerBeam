import 'package:flutter/material.dart';

import '../data/discovery_repository.dart';
import '../data/history_repository.dart';
import '../data/saved_devices_repository.dart';
import '../data/transfer_repository.dart';
import '../data/trust_repository.dart';
import '../sdk/peerbeam.dart';
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
  final SavedDevicesRepository saved;
  final TrustRepository trust;
  final SettingsStore settings;
  final StagingStore staging;

  AppState({
    required this.theme,
    required this.device,
    required this.transfer,
    required this.history,
    required this.saved,
    required this.trust,
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
      saved: SavedDevicesRepository()..load(),
      trust: TrustRepository(api: api),
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
    trust.dispose();
    transfer.dispose();
    history.dispose();
    saved.dispose();
    settings.dispose();
    staging.dispose();
  }
}
