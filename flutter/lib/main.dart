import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'app/router.dart';
import 'app/theme.dart';
import 'platform/android_integration.dart';
import 'platform/bridge.dart';
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
  late final AndroidIntegration _android = AndroidIntegration(
    bridge: AndroidBridge(),
    staging: _state.staging,
    transfer: _state.transfer,
    settings: _state.settings,
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
        await _api.initialize();
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

    // No-op off Android; routes share/receive intents and drives the service.
    _android.start();
  }

  /// Read a received clipboard file (small text) and offer to copy it.
  Future<void> _offerClipboardCopy(({String path, String peer}) c) async {
    const maxBytes = 256 * 1024; // clipboard payloads are small text
    String text;
    try {
      final f = File(c.path);
      if (await f.length() > maxBytes) return;
      text = await f.readAsString();
    } catch (_) {
      return; // unreadable/removed — the file still sits in History
    }
    if (text.trim().isEmpty) return;

    final preview = text.length > 60 ? '${text.substring(0, 60)}…' : text;
    _messengerKey.currentState?.showSnackBar(
      SnackBar(
        duration: const Duration(seconds: 8),
        content: Text('Clipboard from ${c.peer}: $preview'),
        action: SnackBarAction(
          label: 'Copy',
          onPressed: () => Clipboard.setData(ClipboardData(text: text)),
        ),
      ),
    );
  }

  @override
  void dispose() {
    _errSub?.cancel();
    _clipSub?.cancel();
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
