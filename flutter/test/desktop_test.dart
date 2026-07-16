// Picker wiring guards. The native picker can't be driven headlessly, but we
// can prove the entry point is wired on every platform and that an empty or
// cancelled pick is a clean no-op.

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/main.dart';
import 'sdk/fake_peerbeam.dart';
import 'package:peerbeam/platform/desktop_files.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test('isDesktop is false under the test target platform', () {
    // flutter_test defaults to a non-desktop target, so desktop-only paths
    // (drop zone, save-dir dialog) stay gated off.
    expect(isDesktop, isFalse);
  });

  testWidgets('Send files opens the picker; a cancelled pick is a no-op', (
    tester,
  ) async {
    // The test target platform is Android (flutter_test forces
    // defaultTargetPlatform to android — see AndroidBridge._enabled), so
    // pickFilesToStage takes the native peerbeam/android picker branch
    // rather than file_selector. Stub it to behave like a cancelled pick.
    final calls = <String>[];
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      const MethodChannel('peerbeam/android'),
      (call) async {
        if (call.method == 'pickFiles') calls.add(call.method);
        return null; // no files chosen
      },
    );

    await tester.pumpWidget(PeerBeamApp(api: FakePeerBeam()));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    final sendFiles = find.text('Send files');
    expect(sendFiles, findsOneWidget);
    await tester.tap(sendFiles);
    await tester.pump();

    // The picker was invoked (no platform gate) and nothing crashed or opened.
    expect(calls, isNotEmpty);
    expect(find.text('Ready to send'), findsNothing);
    expect(tester.takeException(), isNull);
  }, skip: kIsWeb);
}
