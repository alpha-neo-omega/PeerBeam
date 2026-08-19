import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/main.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

void main() {
  testWidgets(
    'a ring shows a banner naming who is looking, anywhere in the app',
    (tester) async {
      // The whole point is that the user cannot find the device, so they are not
      // looking at any particular tab — the banner has to sit above the shell.
      final fake = FakePeerBeam();
      await tester.pumpWidget(PeerBeamApp(api: fake));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));

      expect(find.textContaining('is looking for this device'), findsNothing);

      fake.emit(const DeviceRing('pb-bob', 'Bob', 5));
      // The stream delivers asynchronously; one pump lets the listener run and
      // the next rebuilds the banner.
      await tester.pump();
      await tester.pump();

      expect(find.text('Bob is looking for this device'), findsOneWidget);

      // Let the ring expire so no timer outlives the test — and prove it clears
      // itself, which is what stops a device nobody reaches shouting forever.
      await tester.pump(const Duration(seconds: 6));
      expect(find.textContaining('is looking for this device'), findsNothing);
    },
  );

  testWidgets('the banner clears when the user says they found it', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    await tester.pumpWidget(PeerBeamApp(api: fake));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    fake.emit(const DeviceRing('pb-bob', 'Bob', 30));
    await tester.pump();
    await tester.pump();

    await tester.tap(find.text('Found it'));
    await tester.pump();
    expect(find.textContaining('is looking for this device'), findsNothing);
  });

  test('an unnamed ring still names the device by id', () {
    // "A device is looking for this device" is something nobody can act on.
    final alert = RingAlert();
    alert.ring('pb-abc123', 5);
    expect(alert.from, 'pb-abc123');
    alert.dispose();
  });

  test('a second ring extends rather than queueing a second alert', () async {
    // Two rings in a row mean someone is still looking, not that the device
    // owes two separate alerts — the first one's timer must not clear the
    // second.
    final alert = RingAlert();
    alert.ring('Bob', 1);
    alert.ring('Bob', 30);
    await Future<void>.delayed(const Duration(milliseconds: 1200));
    expect(alert.from, 'Bob', reason: 'the first ring expired the extension');
    alert.dispose();
  });
}
