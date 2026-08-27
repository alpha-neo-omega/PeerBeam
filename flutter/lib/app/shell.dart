import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:go_router/go_router.dart';

import '../features/send/drop_zone.dart';
import '../features/transfers/incoming_prompt.dart';
import '../state/app_scope.dart';
import '../sdk/error_text.dart';
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
    // Last, rather than next to Devices where it belongs by subject: this list
    // *is* the Ctrl/⌘+1..N order (see `_withShortcuts`), so anything inserted
    // renumbers every shortcut below it. Chats and Devices each did that once
    // already. Spaces is a place you go to arrange something, not one you flick
    // between, so it takes the free digit at the end instead of shifting four
    // that people have learned.
    _Dest(Icons.workspaces_outlined, Icons.workspaces_rounded, 'Spaces'),
    _Dest(Icons.groups_outlined, Icons.groups_rounded, 'Groups'),
  ];

  /// The destination whose icon carries the active-transfer badge. Derived from
  /// [_destinations] rather than hardcoded: it moved once already when Chats
  /// was inserted ahead of Transfers, and a stale literal here would badge the
  /// wrong tab silently.
  static final int _badgedIndex = _destinations.indexWhere(
    (d) => d.label == 'Transfers',
  );

  void _go(BuildContext context, int index) {
    // **Clear anything pushed over the shell first.** Chat detail, Notes and
    // the timeline are pushed routes rather than branches, and `goBranch`
    // switches the branch *underneath* them — so tapping a destination while
    // one was open changed a screen nobody could see, and the tap read as
    // ignored until the user pressed back. Popping first makes the destination
    // mean what it looks like it means.
    //
    // The **branch's** navigator, not the root one. Each branch of a
    // `StatefulShellRoute` has its own, and a screen pushed from inside a branch
    // — chat detail is pushed from Home — lands there. `goBranch`'s
    // `initialLocation` resets a branch's *declarative* location and leaves an
    // imperatively pushed route sitting on top of it, which is exactly the case
    // here, so it has to be popped explicitly.
    //
    // The root navigator is cleared too, for anything pushed to cover the whole
    // shell (a dialog, a full-screen route). Both are no-ops when there is
    // nothing above the branch's first route.
    final branch =
        navigationShell.route.branches[navigationShell.currentIndex].navigatorKey;
    branch.currentState?.popUntil((route) => route.isFirst);
    final root = Navigator.of(context, rootNavigator: true);
    if (root.canPop()) root.popUntil((route) => route.isFirst);
    navigationShell.goBranch(
      index,
      initialLocation: index == navigationShell.currentIndex,
    );
  }

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
        _EngineBanner(status: state.engine),
        _RingBanner(alert: state.ring),
        Expanded(
          child: DropZone(staging: state.staging, child: navigationShell),
        ),
      ],
    );

    // The incoming-transfer prompt hosts itself here — inside the shell, and so
    // under the **root** navigator — because the decision it raises outranks
    // every screen. Approval used to be offered only on Transfers and in a
    // chat, which meant a file arriving while the user was anywhere else waited
    // with nothing on screen to ask about it.
    //
    // It renders nothing of its own: it wraps the body, listens, and pushes a
    // dialog when one is warranted. Wrapping the *body* rather than the
    // Scaffold keeps the two layout branches below identical.
    final watched = IncomingTransferPrompt(child: body);

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
        context,
        Scaffold(
          body: watched,
          bottomNavigationBar: NavigationBar(
            selectedIndex: index,
            onDestinationSelected: (i) => _go(context, i),
            // Icons only, overriding the theme's `onlyShowSelected`, because
            // seven destinations do not leave room for a word. Each one gets
            // width/7 — about 51px on a 360px phone — and `NavigationBar` lays
            // its label out inside exactly that, with no maxLines to stop it:
            // "Transfers" wrapped to three lines, and because the bar's height
            // is fixed at 68 the layout pushed the icon out of the top of the
            // bar and the label off the bottom of the screen. Nothing threw —
            // it just drew outside itself, which is why this survived six
            // destinations and was only noticed at seven.
            //
            // The label is not lost: `NavigationDestination.tooltip` is set to
            // it below, and the bar keeps the label in the semantics tree
            // whatever this behaviour is (`FadeTransition` with
            // `alwaysIncludeSemantics`), so a screen reader still names every
            // destination. The rail keeps its text — from `Breakpoints.compact`
            // up there is room for it.
            labelBehavior: NavigationDestinationLabelBehavior.alwaysHide,
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
      context,
      Scaffold(
        body: Row(
          children: [
            NavigationRail(
              selectedIndex: index,
              onDestinationSelected: (i) => _go(context, i),
              extended: extended,
              // Seven destinations do not fit a short window: a phone in
              // landscape, or a desktop window tiled to half a screen, pushed the
              // last ones past the bottom edge, where they were unreachable
              // rather than merely ugly. Scrolling the group costs nothing when
              // it does fit.
              scrollable: true,
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
            Expanded(child: watched),
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
  /// by mouse and not by keyboard, with nothing failing to say so. `digit7`
  /// arrives with Spaces for that same reason, and Spaces sits at the end of
  /// `_destinations` precisely so that it is the only digit this change adds
  /// rather than the fourth it moves.
  Widget _withShortcuts(BuildContext context, Widget child) {
    const keys = [
      LogicalKeyboardKey.digit1,
      LogicalKeyboardKey.digit2,
      LogicalKeyboardKey.digit3,
      LogicalKeyboardKey.digit4,
      LogicalKeyboardKey.digit5,
      LogicalKeyboardKey.digit6,
      LogicalKeyboardKey.digit7,
    ];
    // Never bind past the end of either list: a digit with no destination
    // would jump to a branch `goBranch` does not have.
    final n = keys.length < _destinations.length
        ? keys.length
        : _destinations.length;
    final bindings = <ShortcutActivator, VoidCallback>{};
    for (var i = 0; i < n; i++) {
      bindings[SingleActivator(keys[i], control: true)] = () => _go(context, i);
      bindings[SingleActivator(keys[i], meta: true)] = () => _go(context, i);
    }
    return PopScope(
      canPop: navigationShell.currentIndex == 0,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop) _go(context, 0); // return to Home before allowing exit
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
/// Says so when the engine never came up.
///
/// Shown once, in the shell, rather than by rewriting eleven empty states: the
/// failure is the same on every screen, and a message that appears wherever the
/// user happens to be beats one they have to navigate to. Silence here is what
/// made a dead engine indistinguishable from an idle network.
class _EngineBanner extends StatelessWidget {
  final EngineStatus status;
  const _EngineBanner({required this.status});

  @override
  Widget build(BuildContext context) => AnimatedBuilder(
    animation: status,
    builder: (context, _) {
      final failure = status.failure;
      if (failure == null) return const SizedBox.shrink();
      final scheme = Theme.of(context).colorScheme;
      return Material(
        color: scheme.errorContainer,
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
                  Icons.error_outline_rounded,
                  color: scheme.onErrorContainer,
                ),
                const Gap(AppSpace.sm),
                Expanded(
                  child: Text(
                    // Named as the engine's problem, so nobody spends the
                    // evening on their router. The detail comes from the same
                    // friendly text every other failure uses.
                    'PeerBeam’s engine did not start — ${friendlyError(failure)}',
                    style: TextStyle(color: scheme.onErrorContainer),
                  ),
                ),
              ],
            ),
          ),
        ),
      );
    },
  );
}

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
