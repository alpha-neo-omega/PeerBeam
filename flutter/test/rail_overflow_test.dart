import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/main.dart';
import 'sdk/fake_peerbeam.dart';

/// **Seven destinations do not fit a short window.** The navigation rail lays
/// its destinations out in a column with no scrolling of its own, so on a phone
/// held in landscape — or a desktop window tiled to half a screen — the last
/// ones were pushed past the bottom edge. That is not a cosmetic overflow: a
/// destination below the edge cannot be tapped, so Spaces and Settings simply
/// stopped existing at that size.
void main() {
  testWidgets('the rail survives a landscape phone without overflowing', (
    tester,
  ) async {
    // A common phone in landscape: short enough that seven destinations plus the
    // leading action cannot all be laid out at once.
    tester.view.physicalSize = const Size(880, 380);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    await tester.pumpWidget(PeerBeamApp(api: FakePeerBeam()));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));

    expect(
      tester.takeException(),
      isNull,
      reason:
          'the rail overflowed its window: at this height the last destinations '
          'are laid out past the bottom edge and cannot be reached',
    );
    expect(find.byType(NavigationRail), findsOneWidget);
  });
}
