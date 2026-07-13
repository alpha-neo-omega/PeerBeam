// Regression tests — pin two real bugs fixed during UI bring-up:
//  1. StatusDot for an *offline* device crashed on dispose (the pulse
//     AnimationController was lazily `late`-initialised and only for online
//     dots, so disposing an offline dot created a ticker on a dead ancestor).
//  2. DeviceTile overflowed (RenderFlex) when a device name was long and the
//     tile was laid out in a narrow row.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/state/models.dart';
import 'package:peerbeam/widgets/device_tile.dart';
import 'package:peerbeam/widgets/status_dot.dart';

void main() {
  group('StatusDot dispose regression', () {
    testWidgets('offline dot builds and disposes without crashing',
        (tester) async {
      await tester.pumpWidget(
        const MaterialApp(home: Scaffold(body: StatusDot(online: false))),
      );
      await tester.pump();

      // Replace the subtree → StatusDot.dispose() runs. The old bug threw here.
      await tester.pumpWidget(
        const MaterialApp(home: Scaffold(body: SizedBox.shrink())),
      );
      expect(tester.takeException(), isNull);
    });

    testWidgets('online dot also disposes cleanly', (tester) async {
      await tester.pumpWidget(
        const MaterialApp(home: Scaffold(body: StatusDot(online: true))),
      );
      await tester.pump();
      await tester.pumpWidget(
        const MaterialApp(home: Scaffold(body: SizedBox.shrink())),
      );
      expect(tester.takeException(), isNull);
    });
  });

  testWidgets('DeviceTile does not overflow with a very long name',
      (tester) async {
    final device = Device(
      id: 'dev-long',
      name: 'A' * 80, // pathologically long, no spaces to wrap on
      kind: DeviceKind.laptop,
      online: true,
      reach: {Reach.lan, Reach.tailscale},
      latencyMs: 12,
    );

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: Center(
            child: SizedBox(width: 160, child: DeviceTile(device: device)),
          ),
        ),
      ),
    );
    await tester.pump();

    // A RenderFlex overflow surfaces as an exception in tests.
    expect(tester.takeException(), isNull);
    expect(find.byType(DeviceTile), findsOneWidget);
  });
}
