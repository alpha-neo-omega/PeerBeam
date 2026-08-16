// UX-polish behaviours: reduced-motion support and keyboard navigation.

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/home/home_screen.dart';
import 'package:peerbeam/main.dart';
import 'sdk/fake_peerbeam.dart';
import 'package:peerbeam/widgets/status_dot.dart';

void main() {
  testWidgets('StatusDot does not pulse when reduced motion is requested', (
    tester,
  ) async {
    await tester.pumpWidget(
      const MaterialApp(
        home: MediaQuery(
          data: MediaQueryData(disableAnimations: true),
          child: Scaffold(body: Center(child: StatusDot(online: true))),
        ),
      ),
    );
    // With reduced motion the pulse never starts, so the tree settles instead
    // of animating forever (pumpAndSettle would time out otherwise).
    await tester.pumpAndSettle();
    expect(tester.takeException(), isNull);
    expect(find.byType(StatusDot), findsOneWidget);
  });

  // The shortcuts are Ctrl/⌘ + N **by position**, so they follow the nav order
  // rather than naming screens. Inserting Chats at position 2 therefore moved
  // Transfers to Ctrl+3, History to Ctrl+4 and Settings to Ctrl+5 — a real
  // behaviour change, asserted here as the whole mapping rather than one digit
  // so the next insertion cannot quietly renumber half of it.
  testWidgets('Ctrl+N switches destinations by position, Chats at 2', (
    tester,
  ) async {
    await tester.pumpWidget(PeerBeamApp(api: FakePeerBeam()));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    Future<void> press(LogicalKeyboardKey digit) async {
      await tester.sendKeyDownEvent(LogicalKeyboardKey.controlLeft);
      await tester.sendKeyEvent(digit);
      await tester.sendKeyUpEvent(LogicalKeyboardKey.controlLeft);
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));
    }

    // Start on Home — none of the other destinations' AppBars are up yet.
    for (final title in ['Chats', 'Transfers', 'History', 'Settings']) {
      expect(find.widgetWithText(AppBar, title), findsNothing);
    }

    final order = <LogicalKeyboardKey, String>{
      LogicalKeyboardKey.digit2: 'Chats',
      LogicalKeyboardKey.digit3: 'Transfers',
      LogicalKeyboardKey.digit4: 'History',
      LogicalKeyboardKey.digit5: 'Settings',
    };
    for (final entry in order.entries) {
      await press(entry.key);
      expect(
        find.widgetWithText(AppBar, entry.value),
        findsOneWidget,
        reason: '${entry.key.keyLabel} must open ${entry.value}',
      );
    }

    // And Ctrl+1 comes back to Home. Asserted on Home's own widget, not on
    // Settings' AppBar being absent: that would pass just as well if Ctrl+1
    // had landed on Chats, Transfers or History — three of the five
    // destinations — which is no assertion about position 1 at all.
    await press(LogicalKeyboardKey.digit1);
    expect(find.byType(HomeScreen), findsOneWidget);
  });

  // The shortcuts above go through `goBranch(i)`, which indexes the ROUTER's
  // branches — so they keep working even if the nav destinations are reordered
  // without the routes. That desync is its own bug, and a nasty one: the item
  // labelled "Chats" would open Settings. So this asserts the other half —
  // that a destination opens the screen its own label names.
  testWidgets('every nav destination opens the screen its label names', (
    tester,
  ) async {
    // Wide enough for the rail, which renders every destination label as text.
    tester.view.physicalSize = const Size(1400, 1000);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    await tester.pumpWidget(PeerBeamApp(api: FakePeerBeam()));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    for (final title in ['Chats', 'Transfers', 'History', 'Settings']) {
      await tester.tap(
        find.descendant(
          of: find.byType(NavigationRail),
          matching: find.text(title),
        ),
      );
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));
      expect(
        find.widgetWithText(AppBar, title),
        findsOneWidget,
        reason: 'the "$title" destination must open the $title screen',
      );
    }
  });
}
