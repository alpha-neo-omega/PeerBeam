import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:window_manager/window_manager.dart';

import 'app/router.dart';
import 'app/theme.dart';
import 'features/chat/chat_screen.dart';
import 'features/groups/group_chat_screen.dart';
import 'features/send/send_text.dart';
import 'features/send/staged_sheet.dart';
import 'platform/android_integration.dart';
import 'platform/bridge.dart';
import 'platform/chat_notifications.dart';
import 'platform/chat_notifier.dart';
import 'platform/desktop_notifier.dart';
import 'platform/engine_config.dart';
import 'platform/desktop_files.dart';
import 'platform/notifications.dart';
import 'platform/saf.dart';
import 'platform/tray.dart';
import 'sdk/models.dart';
import 'sdk/peerbeam.dart';
import 'state/app_scope.dart';
import 'state/stores.dart';

void main() async {
  // `window_manager` has to be initialized before the first frame for
  // `setPreventClose` to be honoured, and that needs the bindings up first.
  // Desktop only: on Android the plugin is not registered and calling it
  // throws, so this is gated rather than merely harmless.
  if (isDesktop) {
    WidgetsFlutterBinding.ensureInitialized();
    await windowManager.ensureInitialized();
  }
  runApp(const PeerBeamApp());
}

/// Root widget. Holds the shared [AppState] + router for the app's lifetime and
/// drives all state from **live engine events** — no mock/sample data.
///
/// Production creates the real [PeerBeam] SDK; tests inject a fake [PeerBeamApi]
/// and drive the same reactive pipeline via events.
class PeerBeamApp extends StatefulWidget {
  /// Engine SDK. When null, the real FFI-backed engine is loaded.
  final PeerBeamApi? api;

  const PeerBeamApp({super.key, this.api});

  @override
  State<PeerBeamApp> createState() => _PeerBeamAppState();
}

class _PeerBeamAppState extends State<PeerBeamApp> with WidgetsBindingObserver {
  late final PeerBeamApi _api;
  late final AppState _state;
  final _router = buildRouter();
  final _messengerKey = GlobalKey<ScaffoldMessengerState>();
  StreamSubscription<String>? _errSub;
  StreamSubscription<({String path, String peer})>? _clipSub;
  StreamSubscription<({String path, String name, String peer})>? _fileSub;
  StreamSubscription<void>? _shareSub;
  StreamSubscription<String>? _clipNoticeSub;
  bool _sheetOpen = false;
  // `api` is what makes the battery reporter exist: `start()` builds it only
  // when there is an engine to push readings into, and this call site never
  // passed one — so Android never reported its battery to a peer, and the Rust
  // side cannot cover for it (`peerbeam-platform`'s reader is Linux-only).
  //
  // Safe as a `late final` initialiser: it is first touched in `initState`
  // after `_api` is assigned.
  late final AndroidIntegration _android = AndroidIntegration(
    bridge: AndroidBridge(),
    staging: _state.staging,
    transfer: _state.transfer,
    settings: _state.settings,
    history: _state.history,
    api: _api,
  );

  /// The tray / menu-bar icon. Null off desktop and in widget tests, where
  /// there is no tray plugin to talk to.
  TrayService? _tray;

  /// Desktop notifications. Constructed everywhere — it is inert off desktop —
  /// so the delivery closures below need no second platform check.
  final DesktopNotifier _desktopNotifier = DesktopNotifier();

  /// Turns arriving messages into notifications. Built in `initState`, where
  /// `_state` exists.
  late final ChatNotifier _chatNotifier;

  @override
  void initState() {
    super.initState();
    _api = widget.api ?? PeerBeam();
    _state = AppState.live(_api);

    // Which thread is open is half of "can the user already see this"; the
    // other half is whether the app has the foreground, which only the binding
    // reports.
    WidgetsBinding.instance.addObserver(this);
    _chatNotifier = ChatNotifier(
      events: _api.events,
      presence: _state.chatPresence,
      enabled: () => _state.settings.notifications,
      nameOf: _peerName,
      groupNameOf: _groupName,
      post: _postChatNotice,
      withdraw: _withdrawChatNotice,
    );
    _chatNotifier.start();

    // Boot the engine, then start discovery so screens fill with live data.
    // Failures (missing native lib) degrade gracefully to empty state.
    () async {
      try {
        // Hand the engine real platform paths (Android needs them; desktop
        // returns '' and keeps the engine's own Downloads/data defaults).
        await _api.initialize(configJson: await buildEngineConfigJson());
        // Persisted settings (device name, save dir, theme, toggles).
        await _state.settings.load(_api);
        _applyPersistedTheme();
        // Load persisted history + trusted devices now that the engine is
        // initialized (fetching them any earlier just hits not_initialised
        // and is swallowed, leaving cold start looking empty).
        await _state.history.refresh();
        await _state.trust.refresh();
        // Transfers the engine's checkpoints say were interrupted — by a
        // dropped link, or by this app being closed mid-flight. Nothing else
        // will ever mention them: the events that would have described them
        // belonged to a process that is gone, so without this fetch a transfer
        // killed by a restart is simply invisible, resumable but unseen.
        await _state.transfer.refreshInterrupted();
        // Presence is live state, so this fetch is only ever "what has already
        // arrived" — normally nothing on a cold start. Refreshing anyway picks
        // up our own sharing flag for the dashboard banner, and any peer that
        // heartbeated while the UI was still booting.
        await _state.presence.refresh();
        // Trusted devices are loaded, so the clipboard watcher has peers to
        // offer to. Started only if the opt-in is on and only on desktop —
        // `applySetting` and `start` both enforce that, so this call is safe
        // unconditionally.
        _state.clipboard?.applySetting(enabled: _state.settings.syncClipboard);
        // No-op off Android; routes share/receive intents and drives the
        // service. Started after history is loaded so the send-notify
        // baseline is seeded from real history, not an empty list — otherwise
        // every historical send would look "new" on cold start.
        await _android.start();
        // The identities earlier sessions resolved by dialling — which peer a
        // `ts:<node>` turned out to be. Read before discovery starts, so a
        // conversation opened in the first seconds already knows how to reach
        // its peer instead of offering a dead composer.
        await _state.device.loadIdentities();
        // Through the repo, so the Scan/Stop control reflects reality.
        await _state.device.start();
        // The tray reads the repositories above, so it is started once they
        // are live — an icon whose menu says "no devices" because discovery
        // had not begun would be wrong for the first few seconds. A no-op off
        // desktop.
        await _startTray();
        // After the tray, and for the same reason: a notification that opens a
        // conversation needs the repositories behind it to be live.
        await _desktopNotifier.start(onOpen: _openConversation);
        _state.engine.started();
      } catch (e) {
        // **Kept, not swallowed.** This used to be `catch (_) {}`, and a failed
        // boot then looked exactly like a quiet network: every screen showed
        // its empty state and the user blamed their wifi for something that
        // never reached the wifi. The shell reads this and says so.
        _state.engine.failed(e);
      }
    }();

    // Surface transfer failures as snackbars (reactive; never polled).
    _errSub = _state.transfer.errors.listen((message) {
      _messengerKey.currentState?.showSnackBar(
        SnackBar(content: Text(message)),
      );
    });

    // A received clipboard payload gets a one-tap Copy — clipboard-to-
    // clipboard instead of a buried .txt file.
    _clipSub = _state.transfer.clipboardReceived.listen(_offerClipboardCopy);

    // Auto-synced clipboards announce themselves. The clipboard changing under
    // the user is the kind of thing they must never have to discover by
    // pasting — but the toast names the sender, never the content, which is on
    // their clipboard already and may well be a password.
    _clipNoticeSub = _state.clipboard?.notices.listen((message) {
      _messengerKey.currentState?.showSnackBar(
        SnackBar(content: Text(message)),
      );
    });

    // Follow the opt-in without an app restart: flipping the switch starts or
    // stops the watcher at once.
    _state.settings.addListener(_applyClipboardSetting);

    // A received file: on Android copy it into the user's chosen folder (the
    // engine's write location is hidden by scoped storage), then drop the copy.
    _fileSub = _state.transfer.fileReceived.listen(_publishReceivedFile);

    // Shared-in content ("Send to PeerBeam"): files open the staged sheet,
    // text offers a one-tap send.
    _shareSub = _android.filesShared.listen((_) => _openStagedSheet());
    _android.sharedText.addListener(_onSharedText);

    // Persist theme choices (the controller itself stays engine-agnostic).
    _state.theme.addListener(_persistTheme);
  }

  /// Foreground state, from the only thing that knows it.
  ///
  /// `resumed` is the one state in which the user is actually looking at this
  /// app: on desktop a window that loses focus reports `inactive`, and on
  /// Android a backgrounded app reports `paused` or `hidden`. Anything but
  /// `resumed` is therefore "they cannot see this", which is exactly the
  /// question a notification has to answer.
  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    _state.chatPresence.setForeground(state == AppLifecycleState.resumed);
  }

  /// What to call a peer, for a notification title. Falls back to the device
  /// id, which `chatNotice` then uses rather than inventing a name.
  String _peerName(String peerId) {
    final known = _state.device.devices
        .where((d) => d.id == peerId)
        .map((d) => d.name)
        .firstOrNull;
    if (known != null && known.isNotEmpty) return known;
    return _state.saved.devices
            .where((d) => d.id == peerId)
            .map((d) => d.name)
            .firstOrNull ??
        peerId;
  }

  /// What to call a group, for a notification title. Falls back to its id.
  String _groupName(String groupId) =>
      _state.groups.groups
          .where((g) => g.id == groupId)
          .map((g) => g.name)
          .firstOrNull ??
      groupId;

  /// Show a chat notification on whichever backend this platform has.
  ///
  /// Android keeps its own: the foreground service and its channels are
  /// hand-written in `android/`, they are what survives the app being
  /// backgrounded, and routing chat through a second notification system would
  /// leave two of them fighting over the same tray. Desktop has no such thing,
  /// so it gets the plugin. Everywhere else — a widget test, most obviously —
  /// this does nothing.
  Future<void> _postChatNotice(ChatNotice notice) async {
    if (isDesktop) {
      await _desktopNotifier.show(notice);
      return;
    }
    if (!Platform.isAndroid) return;
    await _android.bridge.showNotification(
      NotificationContent(
        id: notice.id,
        title: notice.title,
        body: notice.body,
        // The receive icon: a message arriving is incoming, and the alternative
        // is the upload glyph.
        incoming: true,
      ),
    );
  }

  /// Take a chat notification down again.
  Future<void> _withdrawChatNotice(int id) async {
    if (isDesktop) {
      await _desktopNotifier.clear(id);
      return;
    }
    if (!Platform.isAndroid) return;
    await _android.bridge.cancelNotification(id);
  }

  /// Open a conversation because its notification was clicked.
  ///
  /// Desktop only: the Android notification's content intent opens the app
  /// itself, which is what that platform's own notification code has always
  /// done and is not this change's to alter.
  ///
  /// The window is raised first — a thread pushed behind another application
  /// would be a click that appeared to do nothing.
  Future<void> _openConversation(String threadKey) async {
    if (isDesktop) {
      try {
        await windowManager.show();
        await windowManager.focus();
      } catch (_) {
        // A window that will not come forward is not a reason to skip the
        // navigation: the thread is still opened, and it is there when they
        // reach the window themselves.
      }
    }
    final context = rootNavigatorKey.currentContext;
    if (context == null || !context.mounted) return;

    if (threadKey.startsWith('group:')) {
      final id = threadKey.substring('group:'.length);
      final group = _state.groups.groups.where((g) => g.id == id).firstOrNull;
      // A group this device has since left, or one the list has not loaded
      // back yet. Nothing sensible to open, and inventing a placeholder group
      // would offer a composer that sends to nobody.
      if (group == null) return;
      await Navigator.of(context).push(
        MaterialPageRoute(
          builder: (_) => GroupChatScreen(group: group, nameFor: _peerName),
        ),
      );
      return;
    }

    // The same fallback the Conversations list uses: discovery's target when
    // it has one, an address-less placeholder otherwise. The chat screen
    // re-resolves it while open, so a peer that reappears becomes sendable
    // there and then.
    final target =
        _state.device.peerTarget(threadKey) ??
        PeerTarget(
          id: threadKey,
          name: _peerName(threadKey),
          addresses: const [],
          port: 0,
        );
    await Navigator.of(context).push(
      MaterialPageRoute(
        builder: (_) => ChatScreen(peerId: threadKey, peer: target),
      ),
    );
  }

  void _applyPersistedTheme() {
    final mode = switch (_state.settings.theme) {
      'light' => ThemeMode.light,
      'dark' => ThemeMode.dark,
      _ => ThemeMode.system,
    };
    _state.theme.setMode(mode);
  }

  void _persistTheme() => _state.settings.setTheme(_state.theme.mode.name);

  void _applyClipboardSetting() =>
      _state.clipboard?.applySetting(enabled: _state.settings.syncClipboard);

  /// Open the staged-files sheet over the current screen (post-frame so a
  /// cold-start share waits for the first build).
  void _openStagedSheet() {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (_sheetOpen) return; // coalesce re-shares into the open sheet
      final context = rootNavigatorKey.currentContext;
      if (context == null) return;
      _sheetOpen = true;
      showStagedFilesSheet(
        context,
        _state.staging,
      ).whenComplete(() => _sheetOpen = false);
    });
  }

  /// Shared text arrived: add it to the selection stack and open the tray
  /// (same path as any other staged item).
  void _onSharedText() {
    final text = _android.sharedText.value;
    if (text == null || text.trim().isEmpty) return;
    _android.sharedText.value = null; // consume
    _state.staging.addText(text);
    _openStagedSheet();
  }

  /// On Android, copy a freshly received file or folder into the user's chosen
  /// SAF folder (so it's visible in Files/Gallery), drop the engine's private
  /// copy, and surface a "Received `name`" notification.
  /// No-op off Android, or when no folder is chosen yet — the item then stays
  /// in app storage.
  Future<void> _publishReceivedFile(
    ({String path, String name, String peer}) f,
  ) async {
    if (!Saf.isSupported) return;
    try {
      if (FileSystemEntity.isDirectorySync(f.path)) {
        // A received folder: publish the whole tree, then drop the local copy.
        if (await Saf.saveTree(f.path)) {
          await Directory(f.path).delete(recursive: true);
        }
        if (_state.settings.notifications) {
          unawaited(
            _android.bridge.showNotification(
              TransferNotifications.received(f.name, f.peer),
            ),
          );
        }
        return;
      }
      final file = File(f.path);
      if (!await file.exists()) {
        return;
      }
      if (await Saf.save(f.path, f.name)) {
        await file.delete();
      }
      if (_state.settings.notifications) {
        unawaited(
          _android.bridge.showNotification(
            TransferNotifications.received(f.name, f.peer),
          ),
        );
      }
    } catch (_) {
      // Leave the item in app storage if the copy fails.
    }
  }

  /// Read a received text payload and show it as a message dialog (LocalSend
  /// style) — content + Copy — instead of it looking like a downloaded file.
  Future<void> _offerClipboardCopy(({String path, String peer}) c) async {
    // `readMessagePayload` holds the cap, shared with History so the two
    // cannot drift — History had no cap at all.
    final text = await readMessagePayload(c.path);
    if (text == null || text.trim().isEmpty) return;
    _showMessage('Message from ${c.peer}', text);
  }

  /// Present a message over the current screen (synchronous — no BuildContext
  /// held across an async gap; the global-key context is looked up fresh).
  void _showMessage(String title, String text) {
    final context = rootNavigatorKey.currentContext;
    if (context != null) {
      showMessageDialog(context, title: title, text: text);
    }
  }

  /// Install the tray icon, wiring its three actions to the ones the window
  /// already offers. A no-op off desktop, and a failure here never stops boot:
  /// some Linux sessions have no status-notifier host, and the window is the
  /// primary surface regardless.
  Future<void> _startTray() async {
    if (!isDesktop) return;
    try {
      final tray = TrayService(
        state: _state,
        showWindow: () async {
          await windowManager.show();
          await windowManager.focus();
        },
        sendFiles: () async {
          await windowManager.show();
          await windowManager.focus();
          final picked = await pickFilesToStage(keep: _state.staging.paths);
          if (picked.isEmpty) return;
          final added = _state.staging.add(picked);
          // Show the sheet, exactly as Home's own "Send Files" does. Without
          // it the action dead-ended: files were staged and nothing appeared,
          // so unless the user happened to be looking at Home there was no
          // recipient chooser, no confirmation, and no sign anything had
          // happened. The tray's whole point is being usable without the
          // window in front of you.
          final nav = rootNavigatorKey.currentContext;
          if (added > 0 && nav != null && nav.mounted) {
            await showStagedFilesSheet(nav, _state.staging);
          }
        },
        quit: () async {
          // Past the close-to-tray interception, or `destroy` would be caught
          // by the very handler that keeps the app alive.
          await windowManager.setPreventClose(false);
          await windowManager.destroy();
        },
      );
      await tray.start();
      _tray = tray;
    } catch (_) {
      // No tray. The app is otherwise unaffected.
    }
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _chatNotifier.dispose();
    unawaited(_tray?.dispose());
    _errSub?.cancel();
    _clipSub?.cancel();
    _clipNoticeSub?.cancel();
    _state.settings.removeListener(_applyClipboardSetting);
    _fileSub?.cancel();
    _shareSub?.cancel();
    _state.theme.removeListener(_persistTheme);
    _android.sharedText.removeListener(_onSharedText);
    _android.dispose();
    _api.shutdown();
    _state.dispose();
    _router.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AppScope(
      state: _state,
      child: AnimatedBuilder(
        animation: _state.theme,
        builder: (context, _) => MaterialApp.router(
          title: 'PeerBeam',
          debugShowCheckedModeBanner: false,
          scaffoldMessengerKey: _messengerKey,
          theme: PeerBeamTheme.light(),
          darkTheme: PeerBeamTheme.dark(),
          themeMode: _state.theme.mode,
          routerConfig: _router,
        ),
      ),
    );
  }
}
