import 'package:flutter/material.dart';

import 'app/router.dart';
import 'app/theme.dart';
import 'platform/android_integration.dart';
import 'platform/bridge.dart';
import 'state/app_scope.dart';
import 'state/stores.dart';

void main() => runApp(const PeerBeamApp());

/// Root widget. Holds the shared [AppState] and the router for the app's
/// lifetime; rebuilds `MaterialApp` only when the theme mode changes.
class PeerBeamApp extends StatefulWidget {
  const PeerBeamApp({super.key});

  @override
  State<PeerBeamApp> createState() => _PeerBeamAppState();
}

class _PeerBeamAppState extends State<PeerBeamApp> {
  final AppState _state = AppState.sample();
  final _router = buildRouter();
  late final AndroidIntegration _android = AndroidIntegration(
    bridge: AndroidBridge(),
    staging: _state.staging,
    transfer: _state.transfer,
    settings: _state.settings,
  );

  @override
  void initState() {
    super.initState();
    // No-op off Android; routes share/receive intents and drives the service.
    _android.start();
  }

  @override
  void dispose() {
    _android.dispose();
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
          theme: PeerBeamTheme.light(),
          darkTheme: PeerBeamTheme.dark(),
          themeMode: _state.theme.mode,
          routerConfig: _router,
        ),
      ),
    );
  }
}
