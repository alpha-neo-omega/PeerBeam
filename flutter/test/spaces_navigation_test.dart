// Spaces is reachable, and adding it renumbered nothing.
//
// The shell's Ctrl/⌘+1..N shortcuts are positional: the digit follows the nav
// order, so anything inserted mid-list silently moves every shortcut below it.
// Chats did that once and Devices did it again. Spaces is therefore appended,
// and this test pins both halves of that decision — the new destination works,
// and the digit that used to open Settings still does.

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/main.dart';

import 'sdk/fake_peerbeam.dart';

Future<void> _boot(WidgetTester tester) async {
  await tester.pumpWidget(PeerBeamApp(api: FakePeerBeam()));
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 300));
}

void main() {
  testWidgets('the Spaces destination opens the Spaces screen', (tester) async {
    // Wide enough for the rail, which renders every destination as text.
    tester.view.physicalSize = const Size(1400, 1000);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    await _boot(tester);
    await tester.tap(
      find.descendant(
        of: find.byType(NavigationRail),
        matching: find.text('Spaces'),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.widgetWithText(AppBar, 'Spaces'), findsOneWidget);
  });

  testWidgets('Ctrl+7 opens Spaces and Ctrl+6 still opens Settings', (
    tester,
  ) async {
    await _boot(tester);

    Future<void> press(LogicalKeyboardKey digit) async {
      await tester.sendKeyDownEvent(LogicalKeyboardKey.controlLeft);
      await tester.sendKeyEvent(digit);
      await tester.sendKeyUpEvent(LogicalKeyboardKey.controlLeft);
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));
    }

    // Without digit7 the shell's bounds guard would drop the binding in
    // silence, leaving the tab mouse-reachable and keyboard-unreachable.
    await press(LogicalKeyboardKey.digit7);
    expect(find.widgetWithText(AppBar, 'Spaces'), findsOneWidget);

    // The point of appending rather than inserting: nobody's learned digit
    // moved.
    await press(LogicalKeyboardKey.digit6);
    expect(find.widgetWithText(AppBar, 'Settings'), findsOneWidget);
  });

  /// Seventh destination on a phone-width bottom bar. The compact layout is the
  /// one that runs out of room first, and a nav that renders as an overflow
  /// error is a nav nobody can use — so the tap is made where the space is
  /// tightest.
  testWidgets('the bottom bar still fits, and Spaces is tappable on a phone', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(360, 720);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    await _boot(tester);
    expect(find.byType(NavigationBar), findsOneWidget);
    expect(tester.takeException(), isNull);

    await tester.tap(
      find.descendant(
        of: find.byType(NavigationBar),
        matching: find.byIcon(Icons.workspaces_outlined),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.widgetWithText(AppBar, 'Spaces'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  /// **The bar has to stay inside itself, and no exception says when it does
  /// not.**
  ///
  /// Seven destinations give each about 51px on a 360px phone, and
  /// `NavigationBar` lays a destination's label out inside exactly that with no
  /// maxLines to stop it wrapping. Its layout then places icon and label as one
  /// stack centred in a bar whose height the theme fixes at 68 — so a label
  /// that wrapped to three lines pushed the *icon* out through the top of the
  /// bar and its own text off the bottom of the screen. Nothing threw: the bar
  /// simply drew outside itself, which is why six destinations got away with it.
  ///
  /// Asserted on the selected destination, since that is the one whose label is
  /// laid out, and on every destination in turn, because each label is a
  /// different length.
  testWidgets('the selected destination stays inside the bar on a phone', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(360, 720);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    await _boot(tester);

    // Outlined icon to tap with, filled icon to measure once selected — the
    // bar swaps one for the other, and the selected one is the interesting one.
    const destinations = <({IconData unselected, IconData selected})>[
      (
        unselected: Icons.devices_other_outlined,
        selected: Icons.devices_other_rounded,
      ),
      (unselected: Icons.forum_outlined, selected: Icons.forum_rounded),
      (
        unselected: Icons.swap_horiz_outlined,
        selected: Icons.swap_horiz_rounded,
      ),
      (unselected: Icons.history_outlined, selected: Icons.history_rounded),
      (unselected: Icons.settings_outlined, selected: Icons.settings_rounded),
      (
        unselected: Icons.workspaces_outlined,
        selected: Icons.workspaces_rounded,
      ),
    ];
    for (final destination in destinations) {
      final bar = find.byType(NavigationBar);
      await tester.tap(
        find.descendant(of: bar, matching: find.byIcon(destination.unselected)),
      );
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));

      final barRect = tester.getRect(bar);
      final icon = tester.getRect(
        find.descendant(of: bar, matching: find.byIcon(destination.selected)),
      );
      expect(
        icon.top,
        greaterThanOrEqualTo(barRect.top),
        reason: '${destination.selected} was pushed out of the top of the bar',
      );
      expect(
        icon.bottom,
        lessThanOrEqualTo(barRect.bottom),
        reason:
            '${destination.selected} was pushed out of the bottom of the bar',
      );
    }
  });

  /// The other half of that fix: the labels are hidden on the compact bar, so
  /// each destination has to name itself some other way. Both ways it already
  /// had are kept — the long-press tooltip, and the semantics label a screen
  /// reader announces — because an unlabelled row of seven icons is a different
  /// bug, not a fix for this one.
  testWidgets('every compact destination still names itself', (tester) async {
    tester.view.physicalSize = const Size(360, 720);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);
    final semantics = tester.ensureSemantics();

    await _boot(tester);

    for (final label in [
      'Home',
      'Devices',
      'Chats',
      'Transfers',
      'History',
      'Settings',
      'Spaces',
    ]) {
      expect(
        find.byTooltip(label),
        findsOneWidget,
        reason: 'a long press on the $label destination must name it',
      );
      expect(
        find.bySemanticsLabel(RegExp(label)),
        findsWidgets,
        reason: 'a screen reader must still announce $label',
      );
    }
    // Disposed here rather than in a tearDown: the framework checks for leaked
    // handles before tearDowns run, so a deferred dispose fails the test it was
    // meant to keep clean.
    semantics.dispose();
  });
}
