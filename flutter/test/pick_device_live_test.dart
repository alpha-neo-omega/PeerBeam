// The picker must follow discovery while it is open.
//
// Both device lists used to be read once, before the sheet opened, and the
// builder closed over that snapshot. Discovery is asynchronous and usually
// still running in the first seconds after launch — so dropping a file straight
// away opened a sheet saying "No devices to send to" that *stayed* wrong for as
// long as it was open, while the device sat visible on Home behind it.
//
// This is the one screen standing between seeing a device and sending to it,
// which is the whole promise of the app.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/features/send/pick_device.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'sdk/fake_peerbeam.dart';

void main() {
  // `SavedDevicesRepository.add` persists through SharedPreferences, whose
  // platform channel never answers in a widget test unless it is primed. Without
  // this the await simply never returns and the test hangs rather than failing.
  setUp(() => SharedPreferences.setMockInitialValues({}));

  testWidgets('a device found after the sheet opens appears in it', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    final state = AppState.live(fake);
    addTearDown(state.dispose);

    await tester.pumpWidget(
      AppScope(
        state: state,
        child: MaterialApp(
          home: Builder(
            builder: (context) => Scaffold(
              body: ElevatedButton(
                onPressed: () => showDevicePicker(context),
                child: const Text('pick'),
              ),
            ),
          ),
        ),
      ),
    );

    // Nothing discovered yet — the state a fresh launch is actually in.
    await tester.tap(find.text('pick'));
    // Fixed pumps rather than `pumpAndSettle`: the app state keeps timers
    // running, so "settled" never arrives and the test would sit until the
    // suite's timeout instead of failing usefully.
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(find.text('No devices to send to'), findsOneWidget);

    // A destination becomes available while the sheet is still open. Saved
    // devices stand in for discovery here because both feed the same sheet
    // through the same `Listenable.merge` — the property under test is that it
    // follows a notifier at all, which the snapshot version did not.
    await state.saved.add(name: 'Bob Laptop', host: '192.168.1.9', port: 49600);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));

    expect(
      find.text('Bob Laptop'),
      findsOneWidget,
      reason: 'the sheet did not follow the device list while open',
    );
    expect(find.text('No devices to send to'), findsNothing);
  });
}
