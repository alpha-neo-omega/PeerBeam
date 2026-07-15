// Smoke test: the app boots and renders LIVE devices from the engine event
// stream (no sample data) — proving the reactive pipeline end to end.

import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/main.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/widgets/brand_mark.dart';

import 'sdk/fake_peerbeam.dart';

void main() {
  testWidgets('boots to Home and shows a device from a live event', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    await tester.pumpWidget(PeerBeamApp(api: fake));
    // Not pumpAndSettle: presence dots pulse forever.
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    // Branding is the logo mark (rail/app bar), not a literal word at every
    // width — assert the mark renders.
    expect(find.byType(PeerBeamMark), findsWidgets);
    expect(find.text('Nearby devices'), findsOneWidget);

    // Emit a live discovery event; the UI must react (no polling, no seed).
    fake.emit(
      const DeviceAdded(
        SdkDevice(
          id: 'x1',
          name: 'Live Laptop',
          kind: 'laptop',
          platform: 'linux',
          addresses: ['127.0.0.1'],
          port: 49600,
          online: true,
          latencyMs: 5,
          reachableLan: true,
          reachableRemote: false,
        ),
      ),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.text('Live Laptop'), findsWidgets);
  });
}
