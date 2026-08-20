// "My devices": the group, and the one claim it must never make.
//
// Marking a machine as yours is a **label kept on this device**. It grants
// nothing, widens no permission, and the marked device is never told. That is
// the whole feature, and it is also the easiest thing in the app to imply
// otherwise — the word "mine" invites the reading that the device now has
// standing it did not have before. So the tests below defend three properties:
//
//  1. **The wording denies granting anything**, in the group heading and again
//     in the confirmation of a mark — the two moments a user reads it.
//  2. **It never looks like a permission control.** A switch, or a home next to
//     the per-device permission switches in Settings, would say "you granted
//     something" whatever the label said.
//  3. **A failed read of the labels is not "nothing is marked".** The grouping
//     is a claim about a list the engine keeps; making that claim from a read
//     that did not answer is the bug class this app has fixed once already.

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'package:peerbeam/features/devices/devices_screen.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

SdkDevice _device(String id, String name) => SdkDevice(
  id: id,
  name: name,
  kind: 'desktop',
  platform: 'linux',
  addresses: const ['10.0.0.5'],
  port: 9000,
  online: true,
  latencyMs: null,
  reachableLan: true,
  reachableRemote: false,
);

/// Fixed pumps rather than `pumpAndSettle`: the screen shows a spinner while
/// the ownership read is in flight, and an indeterminate progress indicator
/// schedules frames forever — "settled" never arrives and the test would sit
/// there until the suite timed out instead of failing.
Future<void> _settle(WidgetTester tester) async {
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 600));
}

Future<AppState> _open(
  WidgetTester tester,
  FakePeerBeam fake, {
  List<SdkDevice> devices = const [],
}) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  // Emitted before the first frame: the repository subscribed when the state
  // was built, so these arrive with the pump below.
  for (final d in devices) {
    fake.emit(DeviceAdded(d));
  }
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: const MaterialApp(home: DevicesScreen()),
    ),
  );
  await _settle(tester);
  return state;
}

/// Open one device row's menu.
Future<void> _openMenu(WidgetTester tester, String name) async {
  await tester.tap(find.byTooltip('Actions for $name'));
  await _settle(tester);
}

/// Every word currently on screen, for the assertions about what the UI claims.
String _copy(WidgetTester tester) => tester
    .widgetList<Text>(find.byType(Text))
    .map((t) => t.data ?? '')
    .join(' ');

void main() {
  // `SavedDevicesRepository.load` goes through SharedPreferences, whose
  // platform channel never answers in a widget test unless it is primed —
  // without this the await never returns and the test hangs rather than fails.
  setUp(() => SharedPreferences.setMockInitialValues({}));

  testWidgets('marked devices are grouped, and the group denies granting '
      'anything', (tester) async {
    final fake = FakePeerBeam()..mine = {'pb-a'};
    await _open(
      tester,
      fake,
      devices: [
        _device('pb-a', 'Alice Desktop'),
        _device('pb-b', 'Bob Laptop'),
      ],
    );

    expect(find.text('My devices'), findsOneWidget);
    expect(find.text('Other devices'), findsOneWidget);
    final copy = _copy(tester);
    expect(
      copy,
      contains('grants nothing'),
      reason: 'the group has to state that the label conveys no standing',
    );
    expect(copy, contains('widens no permission'));
    expect(
      copy,
      contains('never told'),
      reason: 'the marked device is not informed, and the UI must say so',
    );
  });

  testWidgets('nothing marked reads as the default, with no empty group', (
    tester,
  ) async {
    await _open(tester, FakePeerBeam(), devices: [_device('pb-a', 'Alice')]);

    expect(
      find.text('My devices'),
      findsNothing,
      reason: 'a heading over no cards reads as a section that failed to load',
    );
    final copy = _copy(tester);
    expect(copy, contains('None of these are marked as yours'));
    expect(
      copy,
      contains('grants nothing'),
      reason: 'the claim must be readable before anyone uses the feature',
    );
  });

  testWidgets('marking a device reaches the engine and regroups it', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    await _open(
      tester,
      fake,
      devices: [
        _device('pb-a', 'Alice Desktop'),
        _device('pb-b', 'Bob Laptop'),
      ],
    );

    await _openMenu(tester, 'Bob Laptop');
    await tester.tap(find.text('Mark as mine'));
    await _settle(tester);

    expect(fake.calls, contains('setDeviceMine:pb-b:true'));
    expect(
      fake.calls.where((c) => c == 'myDevices').length,
      greaterThan(1),
      reason: 'the engine list is the authority for the grouping, not the tap',
    );
    expect(find.text('My devices'), findsOneWidget);
    expect(find.text('Other devices'), findsOneWidget);
  });

  testWidgets('the confirmation of a mark denies granting anything', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    await _open(tester, fake, devices: [_device('pb-a', 'Alice Desktop')]);

    await _openMenu(tester, 'Alice Desktop');
    await tester.tap(find.text('Mark as mine'));
    await _settle(tester);

    final snack = tester
        .widgetList<Text>(
          find.descendant(
            of: find.byType(SnackBar),
            matching: find.byType(Text),
          ),
        )
        .map((t) => t.data ?? '')
        .join(' ');
    expect(snack, contains('grants no permission'));
    expect(
      snack,
      contains('is not told'),
      reason:
          'nothing is sent to the device, and the confirmation must not '
          'let the user believe otherwise',
    );
  });

  testWidgets('unmarking drops the label and says nothing else changed', (
    tester,
  ) async {
    final fake = FakePeerBeam()..mine = {'pb-a'};
    await _open(tester, fake, devices: [_device('pb-a', 'Alice Desktop')]);

    await _openMenu(tester, 'Alice Desktop');
    await tester.tap(find.text('Remove from My devices'));
    await _settle(tester);

    expect(fake.calls, contains('setDeviceMine:pb-a:false'));
    expect(find.text('My devices'), findsNothing);
    expect(_copy(tester), contains('never granted anything'));
  });

  testWidgets('a refused mark says why and leaves the grouping alone', (
    tester,
  ) async {
    final fake = FakePeerBeam()..failing.add('setDeviceMine');
    await _open(tester, fake, devices: [_device('pb-a', 'Alice Desktop')]);

    await _openMenu(tester, 'Alice Desktop');
    await tester.tap(find.text('Mark as mine'));
    await _settle(tester);

    expect(find.byType(SnackBar), findsOneWidget);
    expect(
      find.text('My devices'),
      findsNothing,
      reason: 'a write the engine refused must not be shown as having happened',
    );
  });

  testWidgets('an ownership read that failed is stated with a retry, never as '
      'nothing being marked', (tester) async {
    final fake = FakePeerBeam()
      ..mine = {'pb-a'}
      ..failing.add('myDevices');
    await _open(tester, fake, devices: [_device('pb-a', 'Alice Desktop')]);

    expect(find.text('Could not read which devices are yours'), findsOneWidget);
    expect(
      find.text('My devices'),
      findsNothing,
      reason: 'the failed read must not be answered with a grouping',
    );
    expect(
      _copy(tester),
      contains('not a claim that none of them are yours'),
      reason: 'an absence and a failure must not read the same',
    );
    // The device itself is still listed: the list came from the event stream
    // and is perfectly good, so a failed side-read must not hide it.
    expect(find.text('Alice Desktop'), findsOneWidget);

    fake.failing.remove('myDevices');
    await tester.tap(find.text('Try again'));
    await _settle(tester);

    expect(find.text('Could not read which devices are yours'), findsNothing);
    expect(find.text('My devices'), findsOneWidget);
  });

  testWidgets('the grouping is not claimed before the read answers', (
    tester,
  ) async {
    final fake = _GatedMine()..mine = {'pb-a'};
    await _open(tester, fake, devices: [_device('pb-a', 'Alice Desktop')]);

    expect(find.text('Checking which of these are yours…'), findsOneWidget);
    expect(find.text('My devices'), findsNothing);
    expect(find.text('None of these are marked as yours'), findsNothing);
    // The list is visible while the question is open — provisional, not hidden.
    expect(find.text('Alice Desktop'), findsOneWidget);

    fake.gate.complete();
    await _settle(tester);

    expect(find.text('My devices'), findsOneWidget);
  });

  testWidgets('nothing about marking a device is a permission toggle', (
    tester,
  ) async {
    final fake = FakePeerBeam()..mine = {'pb-a'};
    await _open(tester, fake, devices: [_device('pb-a', 'Alice Desktop')]);
    await _openMenu(tester, 'Alice Desktop');

    expect(
      find.byType(Switch),
      findsNothing,
      reason:
          'a switch is how this app grants permissions; the label grants '
          'none, so it must not wear one',
    );
    expect(find.byType(SwitchListTile), findsNothing);
  });

  testWidgets('an unreadable label list disables the mark rather than guessing '
      'at it', (tester) async {
    final fake = FakePeerBeam()..failing.add('myDevices');
    await _open(tester, fake, devices: [_device('pb-a', 'Alice Desktop')]);
    await _openMenu(tester, 'Alice Desktop');

    // Offering "Mark as mine" for a device that may already be marked would
    // either do nothing the user can explain, or silently unmark it.
    expect(
      find.text('PeerBeam could not read which devices are yours.'),
      findsOneWidget,
      reason: 'the disabled entry has to say why it is disabled',
    );
  });
}

/// A fake whose ownership read waits to be released, so the screen's
/// in-flight state can be asserted on a frame that is guaranteed to exist.
class _GatedMine extends FakePeerBeam {
  final Completer<void> gate = Completer<void>();

  @override
  Future<List<String>> myDevices() async {
    await gate.future;
    return super.myDevices();
  }
}
