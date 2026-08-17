import 'dart:async';

import 'package:flutter/material.dart';

import '../data/chat_repository.dart';
import '../data/discovery_repository.dart';
import '../data/history_repository.dart';
import '../data/presence_repository.dart';
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

  /// "Share device status with trusted devices" — the presence opt-in.
  ///
  /// **Default off.** While it is off this device sends no status at all, to
  /// anyone; it still receives and displays what its peers share. When on,
  /// status goes only to devices in the trust store — that half is not
  /// configurable, here or anywhere.
  bool sharePresence;

  /// Theme preference as persisted ('system' | 'light' | 'dark').
  String theme;

  PeerBeamApi? _api;

  SettingsStore({
    required this.deviceName,
    required this.saveDirectory,
    required this.autoAcceptTrusted,
    required this.notifications,
    required this.compression,
    // Default on so a fresh inbound transfer is shielded from Doze while the
    // app is backgrounded (zero-config receive). Runs a foreground service with
    // a persistent notification; users can turn it off in Settings.
    this.backgroundReceive = true,
    this.theme = 'system',
    // Opt-in: nothing about this device leaves it until the user says so.
    this.sharePresence = false,
  });

  /// Load persisted settings from the engine (call once after initialize).
  /// Later setters persist through the same document, and the engine applies
  /// device name / save dir / auto-accept on next init.
  Future<void> load(PeerBeamApi api) async {
    _api = api;
    try {
      final s = await api.settingsGet();
      deviceName = (s['device_name'] as String?)?.trim().isNotEmpty == true
          ? (s['device_name'] as String).trim()
          : deviceName;
      saveDirectory = (s['transfer_directory'] as String?) ?? saveDirectory;
      autoAcceptTrusted = (s['auto_accept'] as bool?) ?? autoAcceptTrusted;
      notifications = (s['notifications'] as bool?) ?? notifications;
      compression = (s['compression'] as bool?) ?? compression;
      backgroundReceive =
          (s['background_receive'] as bool?) ?? backgroundReceive;
      // Absent -> stays false. A settings document written before this feature
      // existed must never be read as consent.
      sharePresence = (s['share_presence'] as bool?) ?? sharePresence;
      theme = (s['theme'] as String?) ?? theme;
      notifyListeners();
    } catch (_) {
      // Engine unavailable (tests/desktop without lib): keep defaults.
    }
  }

  void _persist(String key, Object value) {
    unawaited(_api?.settingsSet({key: value}).catchError((_) {}));
  }

  void setBackgroundReceive(bool v) {
    backgroundReceive = v;
    _persist('background_receive', v);
    notifyListeners();
  }

  void setDeviceName(String v) {
    deviceName = v;
    _persist('device_name', v);
    notifyListeners();
  }

  void setSaveDirectory(String v) {
    saveDirectory = v;
    _persist('transfer_directory', v);
    notifyListeners();
  }

  void setAutoAccept(bool v) {
    autoAcceptTrusted = v;
    _persist('auto_accept', v);
    notifyListeners();
  }

  /// Turn status sharing on or off.
  ///
  /// The engine re-reads this on every heartbeat, so turning it off stops an
  /// already-connected session's next beat rather than waiting for a
  /// reconnect. Turning it on likewise starts sharing without one.
  void setSharePresence(bool v) {
    sharePresence = v;
    _persist('share_presence', v);
    notifyListeners();
  }

  void setNotifications(bool v) {
    notifications = v;
    _persist('notifications', v);
    notifyListeners();
  }

  void setCompression(bool v) {
    compression = v;
    _persist('compression', v);
    notifyListeners();
  }

  void setTheme(String v) {
    theme = v;
    _persist('theme', v);
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
  final ChatRepository chat;
  final PresenceRepository presence;
  final SettingsStore settings;
  final StagingStore staging;

  AppState({
    required this.theme,
    required this.device,
    required this.transfer,
    required this.history,
    required this.saved,
    required this.trust,
    required this.chat,
    required this.presence,
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
      chat: ChatRepository(api: api),
      presence: PresenceRepository(api: api),
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
    chat.dispose();
    presence.dispose();
    transfer.dispose();
    history.dispose();
    saved.dispose();
    settings.dispose();
    staging.dispose();
  }
}
