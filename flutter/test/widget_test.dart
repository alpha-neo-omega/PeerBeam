// Smoke test: the app boots and shows the Home destination.

import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/main.dart';
import 'package:peerbeam/state/stores.dart';

void main() {
  testWidgets('boots to Home with nearby devices', (tester) async {
    await tester.pumpWidget(PeerBeamApp(state: AppState.sample()));
    // Not pumpAndSettle: the presence dots pulse forever.
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 500));

    expect(find.text('PeerBeam'), findsWidgets);
    expect(find.text('Nearby Devices'), findsOneWidget);
    // Sample device present.
    expect(find.text("Alice's MacBook"), findsWidgets);
  });
}
