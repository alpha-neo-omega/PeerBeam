import 'package:flutter/material.dart';
import 'package:go_router/go_router.dart';

import '../features/chats/chats_screen.dart';
import '../features/devices/devices_screen.dart';
import '../features/history/history_screen.dart';
import '../features/home/home_screen.dart';
import '../features/settings/settings_screen.dart';
import '../features/spaces/spaces_screen.dart';
import '../features/transfers/transfers_screen.dart';
import 'shell.dart';
import 'theme.dart';

/// Root navigator key — lets non-widget layers (share-intent handling) open
/// sheets/dialogs over whatever screen is current.
final GlobalKey<NavigatorState> rootNavigatorKey = GlobalKey<NavigatorState>();

/// Declarative routing with a **state-preserving** indexed shell: each tab
/// keeps its state (scroll, animations, subscriptions) across switches, and
/// every destination is URL-addressable / deep-linkable.
GoRouter buildRouter() {
  return GoRouter(
    navigatorKey: rootNavigatorKey,
    initialLocation: '/home',
    routes: [
      StatefulShellRoute.indexedStack(
        builder: (context, state, navigationShell) =>
            AppShell(navigationShell: navigationShell),
        branches: [
          StatefulShellBranch(
            routes: [
              GoRoute(
                path: '/home',
                pageBuilder: (c, s) => _fade(const HomeScreen()),
              ),
            ],
          ),
          StatefulShellBranch(
            routes: [
              GoRoute(
                path: '/devices',
                pageBuilder: (c, s) => _fade(const DevicesScreen()),
              ),
            ],
          ),
          StatefulShellBranch(
            routes: [
              GoRoute(
                path: '/chats',
                pageBuilder: (c, s) => _fade(const ChatsScreen()),
              ),
            ],
          ),
          StatefulShellBranch(
            routes: [
              GoRoute(
                path: '/transfers',
                pageBuilder: (c, s) => _fade(const TransfersScreen()),
              ),
            ],
          ),
          StatefulShellBranch(
            routes: [
              GoRoute(
                path: '/history',
                pageBuilder: (c, s) => _fade(const HistoryScreen()),
              ),
            ],
          ),
          StatefulShellBranch(
            routes: [
              GoRoute(
                path: '/settings',
                pageBuilder: (c, s) => _fade(const SettingsScreen()),
              ),
            ],
          ),
          // Appended, not slotted in beside Devices where it belongs by
          // subject. Branch order is `AppShell._destinations` order is the
          // Ctrl/⌘+1..N order — all three index-for-index — so inserting
          // renumbers every shortcut after the insertion point. That has
          // happened twice already; a Space is not something anyone reaches by
          // reflex, and it is not worth moving four digits under the fingers of
          // people who do.
          StatefulShellBranch(
            routes: [
              GoRoute(
                path: '/spaces',
                pageBuilder: (c, s) => _fade(const SpacesScreen()),
              ),
            ],
          ),
        ],
      ),
    ],
  );
}

/// Gentle shared-axis-ish fade+scale transition for a native feel.
CustomTransitionPage<void> _fade(Widget child) {
  return CustomTransitionPage<void>(
    child: child,
    transitionDuration: AppMotion.medium,
    transitionsBuilder: (context, animation, secondary, child) {
      final curved = CurvedAnimation(parent: animation, curve: AppMotion.curve);
      return FadeTransition(
        opacity: curved,
        child: FadeTransition(opacity: curved, child: child),
      );
    },
  );
}
