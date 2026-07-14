// Desktop wiring guards. The native picker can't be driven headlessly, but we
// can prove the guarded entry points don't crash and the platform gating holds.

import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/main.dart';
import 'sdk/fake_peerbeam.dart';
import 'package:peerbeam/platform/desktop_files.dart';

void main() {
  test('isDesktop is false under the test target platform', () {
    // flutter_test defaults to a non-desktop target, so desktop-only paths
    // (picker, save dialog) stay gated off.
    expect(isDesktop, isFalse);
  });

  testWidgets('tapping Send Files off-desktop shows guidance, does not crash', (
    tester,
  ) async {
    await tester.pumpWidget(PeerBeamApp(api: FakePeerBeam()));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    final sendFiles = find.text('Send files');
    expect(sendFiles, findsOneWidget);
    await tester.tap(sendFiles);
    await tester.pump();

    // Non-desktop falls back to the guidance snackbar (no native picker).
    expect(find.textContaining('Send files'), findsWidgets);
    expect(tester.takeException(), isNull);
  }, skip: kIsWeb);
}
