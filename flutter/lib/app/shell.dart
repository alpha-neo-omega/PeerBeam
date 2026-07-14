import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:go_router/go_router.dart';

import '../features/send/drop_zone.dart';
import '../state/app_scope.dart';
import '../widgets/brand_mark.dart';
import 'theme.dart';

/// Responsive application shell. Chooses the navigation affordance by window
/// width — bottom bar (compact), rail (medium), extended rail (expanded) —
/// while the [navigationShell] keeps every tab's state alive.
class AppShell extends StatelessWidget {
  final StatefulNavigationShell navigationShell;

  const AppShell({super.key, required this.navigationShell});

  static const _destinations = [
    _Dest(Icons.home_outlined, Icons.home_rounded, 'Home'),
    _Dest(Icons.swap_horiz_outlined, Icons.swap_horiz_rounded, 'Transfers'),
    _Dest(Icons.history_outlined, Icons.history_rounded, 'History'),
    _Dest(Icons.settings_outlined, Icons.settings_rounded, 'Settings'),
  ];

  void _go(int index) => navigationShell.goBranch(
    index,
    initialLocation: index == navigationShell.currentIndex,
  );

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    final width = MediaQuery.sizeOf(context).width;
    final index = navigationShell.currentIndex;

    // Desktop-only drag & drop wraps the whole content area.
    final body = DropZone(staging: state.staging, child: navigationShell);

    // Transfer badge count reacts only to the transfer store.
    Widget badgedIcon(Widget icon) => AnimatedBuilder(
      animation: state.transfer,
      builder: (context, _) {
        final n = state.transfer.activeCount;
        return Badge(isLabelVisible: n > 0, label: Text('$n'), child: icon);
      },
    );

    if (width < Breakpoints.compact) {
      return _withShortcuts(
        Scaffold(
          body: body,
          bottomNavigationBar: NavigationBar(
            selectedIndex: index,
            onDestinationSelected: _go,
            destinations: [
              for (var i = 0; i < _destinations.length; i++)
                NavigationDestination(
                  icon: _wrapBadge(
                    i,
                    _destinations[i].iconOf(false),
                    badgedIcon,
                  ),
                  selectedIcon: _wrapBadge(
                    i,
                    _destinations[i].iconOf(true),
                    badgedIcon,
                  ),
                  label: _destinations[i].label,
                  tooltip: _destinations[i].label,
                ),
            ],
          ),
        ),
      );
    }

    final extended = width >= Breakpoints.medium;
    return _withShortcuts(
      Scaffold(
        body: Row(
          children: [
            NavigationRail(
              selectedIndex: index,
              onDestinationSelected: _go,
              extended: extended,
              labelType: extended ? null : NavigationRailLabelType.all,
              leading: _RailLeading(extended: extended),
              destinations: [
                for (var i = 0; i < _destinations.length; i++)
                  NavigationRailDestination(
                    icon: _wrapBadge(
                      i,
                      _destinations[i].iconOf(false),
                      badgedIcon,
                    ),
                    selectedIcon: _wrapBadge(
                      i,
                      _destinations[i].iconOf(true),
                      badgedIcon,
                    ),
                    label: Text(_destinations[i].label),
                  ),
              ],
            ),
            const VerticalDivider(width: 1, thickness: 1),
            Expanded(child: body),
          ],
        ),
      ),
    );
  }

  Widget _wrapBadge(int i, Widget icon, Widget Function(Widget) badge) =>
      i == 1 ? badge(icon) : icon;

  /// Desktop keyboard navigation: Ctrl/⌘ + 1..4 jumps to a destination.
  Widget _withShortcuts(Widget child) {
    const keys = [
      LogicalKeyboardKey.digit1,
      LogicalKeyboardKey.digit2,
      LogicalKeyboardKey.digit3,
      LogicalKeyboardKey.digit4,
    ];
    final bindings = <ShortcutActivator, VoidCallback>{};
    for (var i = 0; i < keys.length; i++) {
      bindings[SingleActivator(keys[i], control: true)] = () => _go(i);
      bindings[SingleActivator(keys[i], meta: true)] = () => _go(i);
    }
    return CallbackShortcuts(
      bindings: bindings,
      child: Focus(autofocus: true, child: child),
    );
  }
}

class _Dest {
  final IconData outline;
  final IconData filled;
  final String label;
  const _Dest(this.outline, this.filled, this.label);
  Widget iconOf(bool selected) => Icon(selected ? filled : outline);
}

class _RailLeading extends StatelessWidget {
  final bool extended;
  const _RailLeading({required this.extended});

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.symmetric(
        vertical: AppSpace.lg,
        horizontal: AppSpace.sm,
      ),
      child: BrandLockup(showWordmark: extended),
    );
  }
}
