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
}
