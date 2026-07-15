import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/features/home/home_screen.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/staging.dart';
import 'package:peerbeam/state/stores.dart';
import 'sdk/fake_peerbeam.dart';

void main() {
  testWidgets('persistent selection bar appears when the stack is non-empty', (
    tester,
  ) async {
    final state = AppState.live(FakePeerBeam());
    await tester.pumpWidget(
      AppScope(state: state, child: const MaterialApp(home: HomeScreen())),
    );
    await tester.pump();

    // Empty stack → no bar.
    expect(find.textContaining('item'), findsNothing);

    state.staging.add([
      StagedFile(path: '/x/a.bin', name: 'a.bin', size: 5),
    ]);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 200)); // AnimatedSize

    // Non-empty stack → the bar shows the count.
    expect(find.textContaining('1 item'), findsOneWidget);
  });
}
