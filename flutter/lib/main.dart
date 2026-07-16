import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';

import 'app/router.dart';
import 'app/theme.dart';
import 'features/send/send_text.dart';
import 'features/send/staged_sheet.dart';
import 'platform/android_integration.dart';
import 'platform/bridge.dart';
import 'platform/engine_config.dart';
import 'platform/notifications.dart';
import 'platform/saf.dart';
import 'sdk/peerbeam.dart';
import 'state/app_scope.dart';
import 'state/stores.dart';

void main() => runApp(const PeerBeamApp());

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

class _PeerBeamAppState extends State<PeerBeamApp> {
  late final PeerBeamApi _api;
  late final AppState _state;
  final _router = buildRouter();
  final _messengerKey = GlobalKey<ScaffoldMessengerState>();
  StreamSubscription<String>? _errSub;
  StreamSubscription<({String path, String peer})>? _clipSub;
  StreamSubscription<({String path, String name, String peer})>? _fileSub;
  StreamSubscription<void>? _shareSub;
  late final AndroidIntegration _android = AndroidIntegration(
    bridge: AndroidBridge(),
    staging: _state.staging,
    transfer: _state.transfer,
    settings: _state.settings,
    history: _state.history,
  );

  @override
  void initState() {
    super.initState();
    _api = widget.api ?? PeerBeam();
    _state = AppState.live(_api);

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
        // Through the repo, so the Scan/Stop control reflects reality.
        await _state.device.start();
      } catch (_) {}
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

    // A received file: on Android copy it into the user's chosen folder (the
    // engine's write location is hidden by scoped storage), then drop the copy.
    _fileSub = _state.transfer.fileReceived.listen(_publishReceivedFile);

    // Shared-in content ("Send to PeerBeam"): files open the staged sheet,
    // text offers a one-tap send.
    _shareSub = _android.filesShared.listen((_) => _openStagedSheet());
    _android.sharedText.addListener(_onSharedText);

    // Persist theme choices (the controller itself stays engine-agnostic).
    _state.theme.addListener(_persistTheme);

    // No-op off Android; routes share/receive intents and drives the service.
    _android.start();
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

  /// Open the staged-files sheet over the current screen (post-frame so a
  /// cold-start share waits for the first build).
  void _openStagedSheet() {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      final context = rootNavigatorKey.currentContext;
      if (context == null) return;
      showStagedFilesSheet(context, _state.staging);
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
        unawaited(
          _android.bridge.showNotification(
            TransferNotifications.received(f.name, f.peer),
          ),
        );
        return;
      }
      final file = File(f.path);
      if (!await file.exists()) {
        return;
      }
      if (await Saf.save(f.path, f.name)) {
        await file.delete();
      }
      unawaited(
        _android.bridge.showNotification(
          TransferNotifications.received(f.name, f.peer),
        ),
      );
    } catch (_) {
      // Leave the item in app storage if the copy fails.
    }
  }

  /// Read a received text payload and show it as a message dialog (LocalSend
  /// style) — content + Copy — instead of it looking like a downloaded file.
  Future<void> _offerClipboardCopy(({String path, String peer}) c) async {
    const maxBytes = 256 * 1024; // text payloads are small
    String text;
    try {
      final f = File(c.path);
      if (await f.length() > maxBytes) return;
      text = await f.readAsString();
    } catch (_) {
      return; // unreadable/removed — the file still sits in History
    }
    if (text.trim().isEmpty) return;
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

  @override
  void dispose() {
    _errSub?.cancel();
    _clipSub?.cancel();
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
