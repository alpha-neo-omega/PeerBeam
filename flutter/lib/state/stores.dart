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

  /// "Tell people when you have read their messages" — the read-receipt opt-in.
  ///
  /// **Default off**, for the same reason [sharePresence] is: a read receipt
  /// discloses when *you* looked, which is a fact about your attention rather
  /// than about the message. While it is off this device sends no receipts at
  /// all; it still applies receipts its peers send, so opting out never costs
  /// you what others choose to tell you.
  bool shareReadReceipts;

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

  /// "Verify new devices with a pairing code" — the first-contact opt-in.
  ///
  /// **Default off**, and unlike [sharePresence] and [syncClipboard] the
  /// default is not about what leaves this device: off simply means an accept
  /// behaves as it always has. On, a transfer from a device pinned by that very
  /// handshake cannot be accepted until the user has confirmed both screens
  /// show the same pairing code.
  ///
  /// The engine, not this flag, is what actually enforces it — this is the copy
  /// the UI renders against, so a stale value can only ever make the app ask
  /// for a confirmation the engine would not have required, never skip one it
  /// would.
  bool requirePairingConfirmation;

  /// Theme preference as persisted ('system' | 'light' | 'dark').
  String theme;

  /// Ordered auto-save rules: **where** a received file lands.
  ///
  /// The order is the tie-break — the first rule that matches a file chooses
  /// its directory — so this list is presented and edited in order, and a file
  /// matching none of them goes to [saveDirectory], exactly as every file did
  /// before rules existed.
  ///
  /// A rule never decides *whether* a file is accepted. That is the approval
  /// prompt and [autoAcceptTrusted], neither of which this touches.
  List<SaveRule> saveRules;

  /// Whether this platform can honour rules at all — reported by the engine,
  /// not guessed here.
  ///
  /// False on Android, which receives into a SAF-granted location and cannot
  /// write to an arbitrary absolute path. The UI must say so rather than offer
  /// an editor that would silently do nothing.
  bool rulesSupported;

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
    this.shareReadReceipts = false,
    // Likewise, and with more at stake — this is the one buffer guaranteed to
    // sometimes hold a password.
    this.syncClipboard = false,
    // Off, so the approval prompt a user already knows does not change until
    // they ask for the extra check.
    this.requirePairingConfirmation = false,
    this.saveRules = const [],
    // Assume not supported until the engine says otherwise, so a surface can
    // never offer the editor on the strength of a failed load.
    this.rulesSupported = false,
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
      shareReadReceipts =
          (s['share_read_receipts'] as bool?) ?? shareReadReceipts;
      // Absent -> stays false, for the same reason: a settings document
      // written before this feature existed is not consent.
      syncClipboard = (s['sync_clipboard'] as bool?) ?? syncClipboard;
      // Absent -> stays false. A settings document written before this check
      // existed must not be read as the user having asked for it.
      requirePairingConfirmation =
          (s['require_pairing_confirmation'] as bool?) ??
          requirePairingConfirmation;
      theme = (s['theme'] as String?) ?? theme;
      // Absent -> no rules, which is the same receive behaviour as before this
      // feature existed. A malformed entry is skipped rather than failing the
      // whole load: an unreadable rule must not blank every other setting.
      saveRules = ((s['save_rules'] as List?) ?? const [])
          .whereType<Map>()
          .map((e) => SaveRule.fromJson(Map<String, dynamic>.from(e)))
          .toList();
      rulesSupported = (s['rules_supported'] as bool?) ?? rulesSupported;
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

  /// Turn read receipts on or off.
  ///
  /// The engine re-reads this every time a receipt would be sent, so turning it
  /// off stops the next one rather than waiting for a reconnect. It governs
  /// sending only — receipts already applied stay applied, and receipts peers
  /// send keep arriving.
  void setShareReadReceipts(bool v) {
    shareReadReceipts = v;
    _persist('share_read_receipts', v);
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

  /// Turn the first-contact pairing check on or off. The engine applies it
  /// live, so the next connection is already covered.
  void setRequirePairingConfirmation(bool v) {
    requirePairingConfirmation = v;
    _persist('require_pairing_confirmation', v);
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

  /// Replace the whole ordered rule list.
  ///
  /// Not `_persist`: rules go through their own engine call, which **validates
  /// them and can refuse**. So this awaits the result and only adopts the new
  /// list once the engine has stored it — an optimistic update here would show
  /// a rule that does not exist, and the user would believe their files were
  /// being sorted. The error is rethrown for the caller to show.
  Future<void> setSaveRules(List<SaveRule> rules) async {
    final api = _api;
    if (api == null) {
      saveRules = List.unmodifiable(rules);
      notifyListeners();
      return;
    }
    await api.rulesSet(rules);
    saveRules = List.unmodifiable(rules);
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

  /// The engine itself, for the few things that are questions about the engine
  /// rather than about any one repository — its version, for instance. Null in
  /// widget tests that build state without one.
  final PeerBeamApi? api;

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
    this.api,
  });

  /// Production wiring: repositories driven by the live engine over [api].
  factory AppState.live(PeerBeamApi api) {
    final device = DiscoveryRepository(api: api);
    final trust = TrustRepository(api: api);
    // Built before the transfer repository so the approval prompt can read the
    // pairing-check setting live — a closure, not a captured value, so turning
    // the check on reaches a prompt that is already on screen.
    final settings = SettingsStore(
      deviceName: 'This Device',
      saveDirectory: '~/Downloads/PeerBeam',
      autoAcceptTrusted: false,
      notifications: true,
      compression: true,
    );
    return AppState(
      api: api,
      theme: ThemeController(),
      device: device,
      transfer: TransferRepository(
        api: api,
        pairingConfirmationRequired: () => settings.requirePairingConfirmation,
      ),
      history: HistoryRepository(api: api),
      saved: SavedDevicesRepository()..load(),
      trust: trust,
      chat: ChatRepository(api: api),
      presence: PresenceRepository(api: api),
      settings: settings,
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
        nameOf: (id) =>
            device.devices
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
