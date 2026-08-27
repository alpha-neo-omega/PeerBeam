import 'dart:async';

import 'package:flutter/material.dart';

import '../data/chat_repository.dart';
import '../data/groups_repository.dart';
import '../data/notes_repository.dart';
import '../data/clipboard_sync.dart';
import '../data/discovery_repository.dart';
import '../data/history_repository.dart';
import '../data/presence_repository.dart';
import '../data/saved_devices_repository.dart';
import '../data/view_prefs_repository.dart';
import '../data/transfer_repository.dart';
import '../data/trust_repository.dart';
import '../sdk/events.dart';
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

  /// "Keep a short clipboard history on this device" — the history opt-in.
  ///
  /// **Default off, and separate from [syncClipboard]**: syncing your clipboard
  /// and keeping a record of it are different decisions. Bundling them would
  /// hand a stored log to someone who only wanted two machines to share a
  /// clipboard, which is exactly what sync promised not to create. History is
  /// bounded, kept only on this device, and never sent to a peer.
  bool clipboardHistory;

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
    this.clipboardHistory = false,
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
      clipboardHistory = (s['clipboard_history'] as bool?) ?? clipboardHistory;
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

  /// Apply a setting locally, persist it, and **put it back if the write is
  /// refused**.
  ///
  /// # Why the old version was worse than an unhandled error
  ///
  /// This used to be `unawaited(_api?.settingsSet(..).catchError((_) {}))` —
  /// fire, forget, and swallow. The switch moved, the store reported the new
  /// value, and nothing anywhere learned that the engine had refused it. For
  /// most settings that is a stale preference; for two of them it is a lie
  /// about the user's security posture. Turning off *Verify new devices with a
  /// pairing code*, or turning off *Sync clipboard with trusted devices*, then
  /// seeing the switch move, is a specific and false assurance.
  ///
  /// The update is still optimistic, because a switch that waits for a
  /// round-trip before moving feels broken. But a refusal reverts it, so what is
  /// on screen is what is actually in force — the rule
  /// [`setSaveRules`](Self::setSaveRules) already followed — and the error is
  /// rethrown so the caller can say so out loud.
  Future<void> _apply<T>(
    String key,
    T value,
    T previous,
    void Function(T) assign,
  ) async {
    assign(value);
    notifyListeners();
    final api = _api;
    if (api == null) return; // No engine (tests, desktop without the lib).
    try {
      await api.settingsSet({key: value as Object});
    } catch (_) {
      assign(previous);
      notifyListeners();
      rethrow;
    }
  }

  Future<void> setBackgroundReceive(bool v) => _apply(
    'background_receive',
    v,
    backgroundReceive,
    (x) => backgroundReceive = x,
  );

  Future<void> setDeviceName(String v) =>
      _apply('device_name', v, deviceName, (x) => deviceName = x);

  Future<void> setSaveDirectory(String v) =>
      _apply('transfer_directory', v, saveDirectory, (x) => saveDirectory = x);

  Future<void> setAutoAccept(bool v) =>
      _apply('auto_accept', v, autoAcceptTrusted, (x) => autoAcceptTrusted = x);

  /// Turn status sharing on or off.
  ///
  /// The engine re-reads this on every heartbeat, so turning it off stops an
  /// already-connected session's next beat rather than waiting for a
  /// reconnect. Turning it on likewise starts sharing without one.
  Future<void> setSharePresence(bool v) =>
      _apply('share_presence', v, sharePresence, (x) => sharePresence = x);

  /// Turn read receipts on or off.
  ///
  /// The engine re-reads this every time a receipt would be sent, so turning it
  /// off stops the next one rather than waiting for a reconnect. It governs
  /// sending only — receipts already applied stay applied, and receipts peers
  /// send keep arriving.
  /// Turn clipboard history on or off.
  ///
  /// Turning it off stops new entries; it does **not** erase what was already
  /// recorded, which is why the settings tile offers a separate Clear.
  Future<void> setClipboardHistory(bool v) => _apply(
    'clipboard_history',
    v,
    clipboardHistory,
    (x) => clipboardHistory = x,
  );

  Future<void> setShareReadReceipts(bool v) => _apply(
    'share_read_receipts',
    v,
    shareReadReceipts,
    (x) => shareReadReceipts = x,
  );

  /// Turn clipboard sync on or off.
  ///
  /// The engine re-reads this on every push, so turning it off stops the next
  /// clip rather than waiting for a reconnect; the desktop watcher starts and
  /// stops with it too, without an app restart.
  Future<void> setSyncClipboard(bool v) =>
      _apply('sync_clipboard', v, syncClipboard, (x) => syncClipboard = x);

  /// Turn the first-contact pairing check on or off. The engine applies it
  /// live, so the next connection is already covered.
  Future<void> setRequirePairingConfirmation(bool v) => _apply(
    'require_pairing_confirmation',
    v,
    requirePairingConfirmation,
    (x) => requirePairingConfirmation = x,
  );

  Future<void> setNotifications(bool v) =>
      _apply('notifications', v, notifications, (x) => notifications = x);

  Future<void> setCompression(bool v) =>
      _apply('compression', v, compression, (x) => compression = x);

  Future<void> setTheme(String v) =>
      _apply('theme', v, theme, (x) => theme = x);

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

/// Whether another device is currently looking for this one.
///
/// Held here rather than in a screen because a ring must be noticeable wherever
/// the user happens to be — the whole point is that they cannot find the
/// device, so they are not looking at any particular tab.
class RingAlert extends ChangeNotifier {
  /// The name of the device asking, or null when nothing is ringing.
  String? from;

  Timer? _timer;

  /// Start (or extend) an alert for [seconds].
  ///
  /// Extending rather than queueing: two rings in a row mean someone is still
  /// looking, not that the device owes two separate alerts.
  void ring(String from, int seconds) {
    this.from = from;
    _timer?.cancel();
    _timer = Timer(Duration(seconds: seconds.clamp(1, 60)), clear);
    notifyListeners();
  }

  /// Stop the alert — on timeout, or because the user found the device and
  /// dismissed it.
  void clear() {
    _timer?.cancel();
    _timer = null;
    if (from == null) return;
    from = null;
    notifyListeners();
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }
}

/// Whether the engine came up, and what went wrong if it did not.
///
/// # Why this has to be visible
///
/// The whole boot sequence in `main.dart` sits inside one `catch (_) {}`. If
/// `initialize()` fails — a missing native library, an engine that refused to
/// start — every screen renders its calm empty state instead: "No nearby
/// devices. Devices on your network appear here." Nothing is wrong with that
/// sentence except that it is not true, and the user blames their network for
/// something that never got as far as the network.
///
/// The honest copy already exists (`sdk/error_text.dart` knows about
/// `NotInitialisedException` and `PeerBeamUnavailable`); nothing was reaching
/// it. This carries the failure to the one place every screen shares.
class EngineStatus extends ChangeNotifier {
  /// True until the boot sequence finishes, either way.
  bool booting = true;

  /// What stopped the engine coming up, or null if nothing did.
  Object? failure;

  /// Whether the engine is up and usable.
  bool get ready => !booting && failure == null;

  /// The engine started.
  void started() {
    booting = false;
    failure = null;
    notifyListeners();
  }

  /// The engine did not start. [error] is kept for the user, not just logged.
  void failed(Object error) {
    booting = false;
    failure = error;
    notifyListeners();
  }
}

/// Top-level container of all state, created once and shared via [AppScope].
class AppState {
  final ThemeController theme;

  /// Whether the engine came up. Screens ask this before blaming the network.
  final EngineStatus engine = EngineStatus();

  final DiscoveryRepository device;
  final TransferRepository transfer;
  final HistoryRepository history;
  final SavedDevicesRepository saved;

  /// How the device lists are displayed. Local to this app, not the engine.
  final ViewPrefsRepository view;
  final TrustRepository trust;
  final ChatRepository chat;
  final NotesRepository notes;

  /// Group conversations, and the invitations waiting for an answer.
  final GroupsRepository groups;

  /// Set while another device is looking for this one.
  final RingAlert ring;
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
    required this.view,
    required this.trust,
    required this.chat,
    required this.notes,
    required this.groups,
    required this.ring,
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
    final ring = RingAlert();
    // Subscribed here rather than in a screen: a ring has to be noticeable
    // wherever the user is, and the reason the device is being rung is that
    // they cannot find it — so they are not looking at any particular tab.
    api.events.listen((e) {
      if (e is DeviceRing) ring.ring(e.deviceName, e.seconds);
    });
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
      view: ViewPrefsRepository()..load(),
      trust: trust,
      chat: ChatRepository(api: api),
      notes: NotesRepository(api: api),
      groups: GroupsRepository(api: api),
      ring: ring,
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
    view.dispose();
    settings.dispose();
    staging.dispose();
    clipboard?.dispose();
  }
}
