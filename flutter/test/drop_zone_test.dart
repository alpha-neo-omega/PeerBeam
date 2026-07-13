import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/features/send/drop_overlay.dart';
import 'package:peerbeam/features/send/drop_zone.dart';
import 'package:peerbeam/state/staging.dart';

void main() {
  testWidgets('is a transparent passthrough on non-desktop platforms', (
    tester,
  ) async {
    // Default test platform is Android → drag & drop disabled; the child
    // renders directly with no drop overlay attached.
    await tester.pumpWidget(
      MaterialApp(
        home: DropZone(staging: StagingStore(), child: const Text('content')),
      ),
    );

    expect(find.text('content'), findsOneWidget);
    expect(find.byType(DropOverlay), findsNothing);
  });

  testWidgets('drop overlay renders its prompt', (tester) async {
    await tester.pumpWidget(
      const MaterialApp(home: Scaffold(body: DropOverlay(active: true))),
    );
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.text('Drop to send'), findsOneWidget);
    expect(find.text('Release to stage your files'), findsOneWidget);
  });
}
