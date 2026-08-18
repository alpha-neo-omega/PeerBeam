import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/browse/browse_screen.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

const _peer = PeerTarget(
  id: 'pb-bob',
  name: 'Bob',
  addresses: ['127.0.0.1'],
  port: 49600,
);

Future<void> _open(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: const MaterialApp(home: BrowseScreen(peer: _peer)),
    ),
  );
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 300));
}

void main() {
  testWidgets('a denied listing names every possible reason, not one', (
    tester,
  ) async {
    // The device sends one answer for every reason. Naming a single cause
    // would invent information the protocol deliberately withholds.
    final fake = FakePeerBeam()..browseDenied = true;
    await _open(tester, fake);

    expect(find.text('Nothing to show'), findsOneWidget);
    expect(find.textContaining('may not share anything here'), findsOneWidget);
    expect(find.textContaining('permission'), findsOneWidget);
  });

  testWidgets('shares are listed and a folder can be opened', (tester) async {
    final fake = FakePeerBeam()
      ..shared[''] = [
        const BrowseEntry(name: 'photos', isDir: true, size: 0),
      ]
      ..shared['photos'] = [
        const BrowseEntry(name: 'holiday.jpg', isDir: false, size: 2048),
      ];
    await _open(tester, fake);

    expect(find.text('photos'), findsOneWidget);
    await tester.tap(find.text('photos'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.text('holiday.jpg'), findsOneWidget);
    expect(find.text('2.0 KB'), findsOneWidget);
    expect(fake.calls, contains('browse:pb-bob:photos'));
  });

  testWidgets('the path shown is share-relative, never a filesystem location', (
    tester,
  ) async {
    // The wire carries share-relative paths precisely so a device's real layout
    // stays its own business; the UI must not reintroduce one.
    final fake = FakePeerBeam()
      ..shared[''] = [const BrowseEntry(name: 'docs', isDir: true, size: 0)]
      ..shared['docs'] = [
        const BrowseEntry(name: 'a.txt', isDir: false, size: 1),
      ];
    await _open(tester, fake);
    await tester.tap(find.text('docs'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.text('docs'), findsOneWidget);
    expect(find.textContaining('/home/'), findsNothing);
    expect(find.textContaining('/Users/'), findsNothing);
  });
}
