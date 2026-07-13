import 'package:flutter/material.dart';

import 'app/router.dart';
import 'app/theme.dart';
import 'platform/android_integration.dart';
import 'platform/bridge.dart';
import 'sdk/peerbeam.dart';
import 'state/app_scope.dart';
import 'state/stores.dart';

void main() => runApp(const PeerBeamApp());

/// Root widget. Holds the shared [AppState] + router for the app's lifetime.
///
/// In production it initialises the Rust engine SDK and drives state from live
/// engine events. Tests inject a seeded [AppState] (and skip the SDK).
class PeerBeamApp extends StatefulWidget {
  /// Inject a pre-built state (tests use `AppState.sample()`); when null, the
  /// app boots the live engine SDK.
  final AppState? state;

  const PeerBeamApp({super.key, this.state});

  @override
  State<PeerBeamApp> createState() => _PeerBeamAppState();
}

class _PeerBeamAppState extends State<PeerBeamApp> {
  PeerBeam? _api;
  late final AppState _state;
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
    if (widget.state != null) {
      _state = widget.state!;
    } else {
      // Production: load the engine SDK and drive state from its events.
      final api = PeerBeam();
      _api = api;
      _state = AppState.live(api);
      // Fire-and-forget init; failures (no native lib) degrade gracefully.
      api.initialize().catchError((_) {});
    }
    // No-op off Android; routes share/receive intents and drives the service.
    _android.start();
  }

  @override
  void dispose() {
    _android.dispose();
    _api?.shutdown();
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
