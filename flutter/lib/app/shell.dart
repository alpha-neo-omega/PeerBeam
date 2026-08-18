import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:go_router/go_router.dart';

import '../features/send/drop_zone.dart';
import '../state/app_scope.dart';
import '../state/stores.dart';
import '../widgets/brand_mark.dart';
import 'theme.dart';

/// Responsive application shell. Chooses the navigation affordance by window
/// width — bottom bar (compact), rail (medium), extended rail (expanded) —
/// while the [navigationShell] keeps every tab's state alive.
class AppShell extends StatelessWidget {
  final StatefulNavigationShell navigationShell;

  const AppShell({super.key, required this.navigationShell});

  /// Nav order, which is also the Ctrl/⌘+1..N order and the branch order in
  /// `buildRouter` — the three are index-for-index and must stay that way.
  static const _destinations = [
    _Dest(Icons.home_outlined, Icons.home_rounded, 'Home'),
    _Dest(Icons.devices_other_outlined, Icons.devices_other_rounded, 'Devices'),
    _Dest(Icons.forum_outlined, Icons.forum_rounded, 'Chats'),
    _Dest(Icons.swap_horiz_outlined, Icons.swap_horiz_rounded, 'Transfers'),
    _Dest(Icons.history_outlined, Icons.history_rounded, 'History'),
    _Dest(Icons.settings_outlined, Icons.settings_rounded, 'Settings'),
  ];

  /// The destination whose icon carries the active-transfer badge. Derived from
  /// [_destinations] rather than hardcoded: it moved once already when Chats
  /// was inserted ahead of Transfers, and a stale literal here would badge the
  /// wrong tab silently.
  static final int _badgedIndex = _destinations.indexWhere(
    (d) => d.label == 'Transfers',
  );

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
    // A ring banner sits above everything: the device is being looked for, so
    // whatever screen happens to be open is the wrong place to hide it.
    final body = Column(
      children: [
        _RingBanner(alert: state.ring),
        Expanded(
          child: DropZone(staging: state.staging, child: navigationShell),
        ),
      ],
    );

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
      i == _badgedIndex ? badge(icon) : icon;

  /// Desktop keyboard navigation: Ctrl/⌘ + 1..N jumps to a destination, by
  /// position — so the digits follow [_destinations] rather than naming
  /// screens. Inserting Chats at position 2 moved Transfers to Ctrl+3, History
  /// to Ctrl+4 and Settings to Ctrl+5; inserting Devices at position 2 has now
  /// shifted each of those one further, to Ctrl+3..6. `digit6` is added here at
  /// the same time — the guard below silently drops any destination past the
  /// end of this list, so a forgotten digit would leave the new tab reachable
  /// by mouse and not by keyboard, with nothing failing to say so.
  Widget _withShortcuts(Widget child) {
    const keys = [
      LogicalKeyboardKey.digit1,
      LogicalKeyboardKey.digit2,
      LogicalKeyboardKey.digit3,
      LogicalKeyboardKey.digit4,
      LogicalKeyboardKey.digit5,
      LogicalKeyboardKey.digit6,
    ];
    // Never bind past the end of either list: a digit with no destination
    // would jump to a branch `goBranch` does not have.
    final n = keys.length < _destinations.length
        ? keys.length
        : _destinations.length;
    final bindings = <ShortcutActivator, VoidCallback>{};
    for (var i = 0; i < n; i++) {
      bindings[SingleActivator(keys[i], control: true)] = () => _go(i);
      bindings[SingleActivator(keys[i], meta: true)] = () => _go(i);
    }
    return PopScope(
      canPop: navigationShell.currentIndex == 0,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop) _go(0); // return to the Home branch before allowing exit
      },
      child: CallbackShortcuts(
        bindings: bindings,
        child: Focus(autofocus: true, child: child),
      ),
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

/// Shown while another device is looking for this one.
///
/// Deliberately loud and deliberately dismissible: the point is to be found, and
/// the moment the user has the device in their hand the banner is noise. It
/// clears itself when the ring expires, so a device nobody reaches stops
/// shouting on its own.
class _RingBanner extends StatelessWidget {
  final RingAlert alert;
  const _RingBanner({required this.alert});

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: alert,
      builder: (context, _) {
        final from = alert.from;
        if (from == null) return const SizedBox.shrink();
        final scheme = Theme.of(context).colorScheme;
        return Material(
          color: scheme.primaryContainer,
          child: SafeArea(
            bottom: false,
            child: Padding(
              padding: const EdgeInsets.symmetric(
                horizontal: AppSpace.md,
                vertical: AppSpace.sm,
              ),
              child: Row(
                children: [
                  Icon(
                    Icons.notifications_active_rounded,
                    color: scheme.onPrimaryContainer,
                  ),
                  const Gap(AppSpace.sm),
                  Expanded(
                    child: Text(
                      '$from is looking for this device',
                      style: TextStyle(color: scheme.onPrimaryContainer),
                    ),
                  ),
                  TextButton(
                    onPressed: alert.clear,
                    child: const Text('Found it'),
                  ),
                ],
              ),
            ),
          ),
        );
      },
    );
  }
}
