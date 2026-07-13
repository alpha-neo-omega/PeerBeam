// UX-polish behaviours: reduced-motion support and keyboard navigation.

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/main.dart';
import 'package:peerbeam/state/stores.dart';
import 'package:peerbeam/widgets/status_dot.dart';

void main() {
  testWidgets('StatusDot does not pulse when reduced motion is requested',
      (tester) async {
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

  testWidgets('Ctrl+3 keyboard shortcut switches to the History tab',
      (tester) async {
    await tester.pumpWidget(PeerBeamApp(state: AppState.sample()));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    // Start on Home — no History AppBar yet.
    expect(find.widgetWithText(AppBar, 'History'), findsNothing);

    await tester.sendKeyDownEvent(LogicalKeyboardKey.controlLeft);
    await tester.sendKeyEvent(LogicalKeyboardKey.digit3);
    await tester.sendKeyUpEvent(LogicalKeyboardKey.controlLeft);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    // History screen is now shown (its AppBar carries the title).
    expect(find.widgetWithText(AppBar, 'History'), findsOneWidget);
  });
}
