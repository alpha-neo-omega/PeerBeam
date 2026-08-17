import 'dart:async';

import 'package:flutter/material.dart';

import '../data/chat_repository.dart';
import '../data/clipboard_sync.dart';
import '../data/discovery_repository.dart';
import '../data/history_repository.dart';
import '../data/presence_repository.dart';
import '../data/saved_devices_repository.dart';
import '../data/transfer_repository.dart';
import '../data/trust_repository.dart';
import '../sdk/models.dart';
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

  /// "Sync clipboard with trusted devices" — the clipboard opt-in.
  ///
  /// **Default off.** While it is off this device sends no clip at all, to
  /// anyone; it still receives and applies what its peers send. When on,
  /// clipboards go only to devices in the trust store — that half is not
  /// configurable, here or anywhere.
  ///
  /// Only desktop can *send*: Android forbids background clipboard reads. And
  /// there is no password detection — everything copied while this is on is
  /// sent. See the Settings copy, which says so in as many words.
  bool syncClipboard;

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
    // Likewise, and with more at stake — this is the one buffer guaranteed to
    // sometimes hold a password.
    this.syncClipboard = false,
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
      // Absent -> stays false, for the same reason: a settings document
      // written before this feature existed is not consent.
      syncClipboard = (s['sync_clipboard'] as bool?) ?? syncClipboard;
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

  /// Turn clipboard sync on or off.
  ///
  /// The engine re-reads this on every push, so turning it off stops the next
  /// clip rather than waiting for a reconnect; the desktop watcher starts and
  /// stops with it too, without an app restart.
  void setSyncClipboard(bool v) {
    syncClipboard = v;
    _persist('sync_clipboard', v);
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

  /// The desktop clipboard watcher. Null when there is no engine to push
  /// through (widget tests that build state without an API).
  final ClipboardSyncService? clipboard;

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
    this.clipboard,
  });

  /// Production wiring: repositories driven by the live engine over [api].
  factory AppState.live(PeerBeamApi api) {
    final device = DiscoveryRepository(api: api);
    final trust = TrustRepository(api: api);
    return AppState(
      theme: ThemeController(),
      device: device,
      transfer: TransferRepository(api: api),
      history: HistoryRepository(api: api),
      saved: SavedDevicesRepository()..load(),
      trust: trust,
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
      // Offered only to devices that are pinned AND currently addressable.
      // The engine's gate is still authoritative and re-checks trust against
      // the *authenticated* peer after the handshake; narrowing here keeps a
      // copy from dialing every stranger on the network, which on a shared LAN
      // would be both wasteful and a signal of its own.
      clipboard: ClipboardSyncService(
        api: api,
        peers: () => trust.items
            .map((t) => device.peerTarget(t.id))
            .whereType<PeerTarget>()
            .toList(),
        nameOf: (id) => device.devices
            .where((d) => d.id == id)
            .map((d) => d.name)
            .firstOrNull ??
            id,
      ),
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
    clipboard?.dispose();
  }
}
