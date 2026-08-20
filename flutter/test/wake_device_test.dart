// Waking a device: two limits the UI has to carry, and one sentence it must
// never say.
//
//  1. **Local network only.** A magic packet is a broadcast, so it cannot
//     travel over Tailscale, a VPN or the internet. Nothing on PeerBeam's side
//     can change that, which is why the dialog states it before the send and
//     warns outright when the device's only sighting was over a tailnet.
//  2. **Nothing confirms a wake.** The protocol has no reply, so `WakeAttempt`
//     carries what was sent and deliberately no "woken" flag. Every word of the
//     result is therefore about *this* machine, and the device list is named as
//     the only real confirmation. `the result never claims the device woke` is
//     the test that would fail first if that ever slipped.
//
// Around them sit the two refusals worth keeping specific: an unapproved device
// (the engine declines it by name, and so does this dialog, without offering a
// field to fill in first), and an address that is not one — caught in the field
// against the same three shapes the engine accepts, so a typo never becomes a
// round trip.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'package:peerbeam/features/devices/devices_screen.dart';
import 'package:peerbeam/features/devices/mac_address.dart';
import 'package:peerbeam/features/devices/wake_dialog.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/exceptions.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

SdkDevice _device(
  String id,
  String name, {
  bool lan = true,
  bool remote = false,
  String platform = 'linux',
}) => SdkDevice(
  id: id,
  name: name,
  kind: 'desktop',
  platform: platform,
  addresses: const ['10.0.0.5'],
  port: 9000,
  online: true,
  latencyMs: null,
  reachableLan: lan,
  reachableRemote: remote,
);

TrustedDevice _pin(String id, {required bool approved}) => TrustedDevice(
  id: id,
  name: 'Alice Desktop',
  fingerprint: 'ab:cd',
  trustedAt: DateTime(2026, 1, 1),
  approved: approved,
);

/// Fixed pumps, never `pumpAndSettle`: both the screen and this dialog show an
/// indeterminate progress indicator while a read is in flight, and one of those
/// schedules frames for ever — "settled" would never arrive.
Future<void> _settle(WidgetTester tester) async {
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 600));
}

/// Open the wake dialog for [name] through the device row's menu — the real
/// route in, so the menu's own wording is exercised on the way.
Future<void> _openWake(
  WidgetTester tester,
  FakePeerBeam fake, {
  required SdkDevice device,
  String name = 'Alice Desktop',
}) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  fake.emit(DeviceAdded(device));
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: const MaterialApp(home: DevicesScreen()),
    ),
  );
  await _settle(tester);
  await tester.tap(find.byTooltip('Actions for $name'));
  await _settle(tester);
  await tester.tap(find.text('Wake…'));
  await _settle(tester);
}

/// Only the dialog's own words: the screen behind it has copy of its own, and
/// an assertion about what the *result* claims must not be able to pass or fail
/// on it.
String _dialogCopy(WidgetTester tester) => tester
    .widgetList<Text>(
      find.descendant(
        of: find.byType(AlertDialog),
        matching: find.byType(Text),
      ),
    )
    .map((t) => t.data ?? '')
    .join(' ')
    .toLowerCase();

Future<void> _send(WidgetTester tester, String mac) async {
  await tester.enterText(find.byType(TextField), mac);
  await _settle(tester);
  await tester.tap(find.text('Send wake packet'));
  await _settle(tester);
}

void main() {
  setUp(() => SharedPreferences.setMockInitialValues({}));

  group('parseMac', () {
    test('the three shapes the engine accepts all normalise to one', () {
      // Stored and shown in the colon form so the row, the CLI and the engine
      // name the same address the same way.
      expect(parseMac('aa:bb:cc:dd:ee:ff').mac, 'aa:bb:cc:dd:ee:ff');
      expect(parseMac('AA-BB-CC-DD-EE-FF').mac, 'aa:bb:cc:dd:ee:ff');
      expect(parseMac('aabb.ccdd.eeff').mac, 'aa:bb:cc:dd:ee:ff');
      expect(parseMac('  aa:bb:cc:dd:ee:ff  ').mac, 'aa:bb:cc:dd:ee:ff');
    });

    test('a fourth shape is refused with the grouping it is missing', () {
      // Deliberately not accepted-and-reshaped: `wake set` refuses this, and a
      // field that took it would teach a form the CLI does not know.
      final result = parseMac('aabbccddeeff');
      expect(result.mac, isNull);
      expect(result.refusal, contains('pairs'));
    });

    test('mixed separators are refused, naming both', () {
      final result = parseMac('aa:bb-cc:dd-ee:ff');
      expect(result.mac, isNull);
      expect(result.refusal, contains('one separator'));
    });

    test('a stray character is named rather than called invalid', () {
      final result = parseMac('aa:bb:cc:dd:ee:gg');
      expect(result.mac, isNull);
      expect(result.refusal, contains('g'));
      expect(result.refusal, contains('hex'));
    });

    test('the wrong number of digits says how many there are', () {
      expect(parseMac('aa:bb:cc:dd:ee').refusal, contains('10'));
      expect(parseMac('').refusal, contains('aa:bb:cc:dd:ee:ff'));
    });
  });

  group('wakeFailureText', () {
    test('the engine declining an unapproved device survives as that reason', () {
      // `friendlyError` renders every invalid_argument as "That action can't be
      // completed", which throws away the only useful half of this answer.
      final text = wakeFailureText(
        const InvalidArgumentException(
          'pb-a is not an approved device, so PeerBeam will not wake it',
        ),
        'Alice Desktop',
      );
      expect(text, contains('Alice Desktop'));
      expect(text, contains('not an approved device'));
    });

    test('a missing address is reported as a missing address', () {
      final text = wakeFailureText(
        const InvalidArgumentException('no address recorded for pb-a'),
        'Alice Desktop',
      );
      expect(text, contains('no hardware address recorded'));
    });

    test('anything else falls back to the shared friendly text', () {
      expect(
        wakeFailureText(StateError('boom'), 'Alice Desktop'),
        'Something went wrong. Please try again.',
      );
    });
  });

  group('the wake dialog', () {
    testWidgets('the result never claims the device woke', (tester) async {
      final fake = FakePeerBeam()..trusted = [_pin('pb-a', approved: true)];
      await _openWake(tester, fake, device: _device('pb-a', 'Alice Desktop'));
      await _send(tester, 'aa:bb:cc:dd:ee:ff');

      final copy = _dialogCopy(tester);
      // What was sent, and where — the only two facts there are.
      expect(copy, contains('aa:bb:cc:dd:ee:ff'));
      expect(copy, contains('255.255.255.255:9'));
      expect(copy, contains('no reply'));
      expect(
        copy,
        contains('device list'),
        reason: 'the only real confirmation has to be named',
      );
      for (final claim in const [
        'woke',
        'woken',
        'awake',
        'success',
        'started',
        'starting',
        'now on',
        'powered on',
        'is up',
      ]) {
        expect(
          copy,
          isNot(contains(claim)),
          reason:
              'nothing replies to a wake packet, so "$claim" would be an '
              'invention',
        );
      }
    });

    testWidgets('the local-network limit is stated before anything is sent', (
      tester,
    ) async {
      final fake = FakePeerBeam()..trusted = [_pin('pb-a', approved: true)];
      await _openWake(tester, fake, device: _device('pb-a', 'Alice Desktop'));

      final copy = _dialogCopy(tester);
      expect(copy, contains('local network only'));
      expect(copy, contains('tailscale'));
      expect(copy, contains('vpn'));
      expect(
        copy,
        contains('nothing confirms a wake'),
        reason: 'the expectation is set before the packet, not after it',
      );
      expect(
        fake.calls.where((c) => c.startsWith('wakeDevice')),
        isEmpty,
        reason: 'opening the dialog must not send anything',
      );
    });

    testWidgets('a device seen only over Tailscale is warned that a broadcast '
        'cannot follow that path', (tester) async {
      final fake = FakePeerBeam()..trusted = [_pin('pb-a', approved: true)];
      await _openWake(
        tester,
        fake,
        device: _device('pb-a', 'Alice Desktop', lan: false, remote: true),
      );

      final copy = _dialogCopy(tester);
      expect(copy, contains('last seen over tailscale'));
      expect(copy, contains('cannot follow that path'));
      // Warned, not blocked: a machine last seen over a tailnet may be asleep
      // on this very network, and refusing would be a claim about where the
      // hardware is that nothing here can support.
      expect(find.text('Send wake packet'), findsOneWidget);
    });

    testWidgets('an unapproved device is refused by name, with nothing sent', (
      tester,
    ) async {
      final fake = FakePeerBeam()..trusted = [_pin('pb-a', approved: false)];
      await _openWake(tester, fake, device: _device('pb-a', 'Alice Desktop'));

      expect(
        _dialogCopy(tester),
        contains('alice desktop is not an approved device'),
        reason:
            'the engine refuses it by name; a generic error would leave '
            'the user with nothing to act on',
      );
      expect(
        find.byType(TextField),
        findsNothing,
        reason: 'no point typing an address for a wake that will not happen',
      );
      expect(find.text('Send wake packet'), findsNothing);
      expect(fake.calls.where((c) => c.startsWith('wakeDevice')), isEmpty);
    });

    testWidgets('an approval check that failed does not read as a refusal', (
      tester,
    ) async {
      final fake = _NoTrustList();
      await _openWake(tester, fake, device: _device('pb-a', 'Alice Desktop'));

      final copy = _dialogCopy(tester);
      expect(
        copy,
        contains('could not check whether alice desktop is approved'),
      );
      expect(
        copy,
        isNot(contains('is not an approved device')),
        reason: 'a check that did not answer says nothing about this device',
      );
      expect(find.text('Try again'), findsOneWidget);
      expect(fake.calls.where((c) => c.startsWith('wakeDevice')), isEmpty);
    });

    testWidgets('an address the engine would refuse never reaches it', (
      tester,
    ) async {
      final fake = FakePeerBeam()..trusted = [_pin('pb-a', approved: true)];
      await _openWake(tester, fake, device: _device('pb-a', 'Alice Desktop'));

      await tester.enterText(find.byType(TextField), 'aabbccddeeff');
      await _settle(tester);

      expect(find.textContaining('Group the digits in pairs'), findsOneWidget);
      final button = tester.widget<FilledButton>(
        find.widgetWithText(FilledButton, 'Send wake packet'),
      );
      expect(button.onPressed, isNull);
      await tester.tap(find.text('Send wake packet'));
      await _settle(tester);
      expect(fake.calls.where((c) => c.startsWith('setWakeAddress')), isEmpty);
    });

    testWidgets('the address is recorded, in the engine’s own form, before the '
        'packet goes out', (tester) async {
      final fake = FakePeerBeam()..trusted = [_pin('pb-a', approved: true)];
      await _openWake(tester, fake, device: _device('pb-a', 'Alice Desktop'));
      // The dotted form: normalising is what lets the engine — which stores one
      // canonical shape — record what the user actually typed.
      await _send(tester, 'aabb.ccdd.eeff');

      final wake = fake.calls
          .where(
            (c) => c.startsWith('setWakeAddress') || c.startsWith('wakeDevice'),
          )
          .toList();
      expect(wake, [
        'setWakeAddress:pb-a:aa:bb:cc:dd:ee:ff',
        'wakeDevice:pb-a',
      ]);
    });

    testWidgets('sending again re-sends the packet and nothing else', (
      tester,
    ) async {
      final fake = FakePeerBeam()..trusted = [_pin('pb-a', approved: true)];
      await _openWake(tester, fake, device: _device('pb-a', 'Alice Desktop'));
      await _send(tester, 'aa:bb:cc:dd:ee:ff');

      await tester.tap(find.text('Send again'));
      await _settle(tester);

      expect(fake.calls.where((c) => c == 'wakeDevice:pb-a').length, 2);
      expect(
        fake.calls.where((c) => c.startsWith('setWakeAddress')).length,
        1,
        reason:
            'the address is already recorded; re-writing it is not a resend',
      );
      expect(_dialogCopy(tester), contains('sending again is safe'));
    });

    testWidgets('the recorded address can be forgotten again', (tester) async {
      final fake = FakePeerBeam()..trusted = [_pin('pb-a', approved: true)];
      await _openWake(tester, fake, device: _device('pb-a', 'Alice Desktop'));
      await _send(tester, 'aa:bb:cc:dd:ee:ff');

      await tester.tap(find.text('Forget address'));
      await _settle(tester);

      expect(fake.calls, contains('forgetWakeAddress:pb-a'));
      expect(find.textContaining('Address forgotten'), findsOneWidget);
    });

    testWidgets('a wake the engine refused is reported as that refusal', (
      tester,
    ) async {
      final fake = _RefusingWake()..trusted = [_pin('pb-a', approved: true)];
      await _openWake(tester, fake, device: _device('pb-a', 'Alice Desktop'));
      await _send(tester, 'aa:bb:cc:dd:ee:ff');

      final copy = _dialogCopy(tester);
      expect(copy, contains('not an approved device'));
      expect(
        copy,
        isNot(contains('255.255.255.255')),
        reason:
            'a refused wake sent nothing, so it must not show a destination',
      );
    });

    testWidgets('a phone is told it probably cannot be woken at all', (
      tester,
    ) async {
      final fake = FakePeerBeam()..trusted = [_pin('pb-a', approved: true)];
      await _openWake(
        tester,
        fake,
        device: _device('pb-a', 'Alice Desktop', platform: 'android'),
      );

      expect(
        _dialogCopy(tester),
        contains('do not listen for wake packets'),
        reason: 'sending a packet a phone will ignore wastes the user’s time',
      );
    });
  });
}

/// A fake whose trust list cannot be read, standing in for an unreadable trust
/// store. `FakePeerBeam.failing` does not cover `trustList`, and the state this
/// produces — "we could not ask" — is the one that must not be shown as "not
/// approved".
class _NoTrustList extends FakePeerBeam {
  @override
  Future<List<TrustedDevice>> trustList() async {
    calls.add('trustList');
    throw const InternalException('trust store unreadable');
  }
}

/// The engine declining a wake for an unapproved device, as it does by name.
/// Reachable in the app only when approval is revoked between the check and the
/// send — which is exactly why the send path re-states the refusal rather than
/// trusting the check it already made.
class _RefusingWake extends FakePeerBeam {
  @override
  Future<WakeAttempt> wakeDevice(String device) async {
    calls.add('wakeDevice:$device');
    throw const InvalidArgumentException(
      'pb-a is not an approved device, so PeerBeam will not wake it',
    );
  }
}
