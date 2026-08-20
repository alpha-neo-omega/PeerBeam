// Filling, emptying and deleting a Space.
//
// The rules this pins are the ones `docs/SPACES.md` calls refusals, and each of
// them is a way the UI could otherwise write a cheque the engine will bounce:
//
//  1. **You cannot add a device you have not paired with.** There is no
//     discovery in a Space and no directory to be found in, so the picker is
//     the trust store and nothing wider. A row whose only possible outcome is
//     "that device is not trusted" is a row that should not be there.
//  2. **Removal validates nothing.** A revoked device is the one you most need
//     to be able to take out, so nothing gates it — but the undo is only
//     offered where it can work, because the engine will not re-add a device it
//     holds no live pin for.
//  3. **Deleting is undone, not confirmed.** A Space is a name and a list of
//     device ids, all of it on screen, so putting it back is exact. Nothing is
//     destroyed elsewhere either: the devices keep their trust, and no peer
//     ever knew the Space existed.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'package:peerbeam/features/spaces/spaces_screen.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

TrustedDevice _pin(String id, String name) => TrustedDevice(
  id: id,
  name: name,
  fingerprint: 'AA:BB',
  trustedAt: DateTime(2026),
  approved: true,
);

/// A device on the network that this machine has never trusted.
SdkDevice _stranger(String id, String name) => SdkDevice(
  id: id,
  name: name,
  kind: 'desktop',
  platform: 'linux',
  addresses: const ['10.0.0.9'],
  port: 5310,
  online: true,
  latencyMs: 4,
  reachableLan: true,
  reachableRemote: false,
);

Future<AppState> _open(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: const MaterialApp(home: SpacesScreen()),
    ),
  );
  await tester.pump();
  await state.trust.refresh();
  await tester.pumpAndSettle();
  return state;
}

String _copy(WidgetTester tester) => tester
    .widgetList<Text>(find.byType(Text))
    .map((t) => t.data ?? '')
    .join(' ');

/// The "Add a device" button on one Space's card.
Finder _addTo(String spaceId) => find.byKey(Key('space-$spaceId-add'));

/// One device row's remove button — never `find.byTooltip` alone, which matches
/// every row the moment a Space holds two devices.
Finder _takeOut(String spaceId, String deviceId) => find.descendant(
  of: find.byKey(Key('space-$spaceId-device-$deviceId')),
  matching: find.byTooltip('Take out of this Space'),
);

/// Open one Space's menu and choose an item from it.
Future<void> _menu(WidgetTester tester, String item) async {
  await tester.tap(find.byTooltip('Space options'));
  await tester.pumpAndSettle();
  await tester.tap(find.text(item));
  await tester.pumpAndSettle();
}

void main() {
  setUp(() => SharedPreferences.setMockInitialValues({}));

  /// **The picker is the trust store.** A device visible on the network but
  /// never paired with cannot be a member — the engine refuses the id — so
  /// offering it would manufacture a failure out of a device sitting right
  /// there on the Devices screen.
  testWidgets('a device on the network but never trusted is not offered', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..trusted = [_pin('pb-a', 'laptop'), _pin('pb-b', 'desktop')]
      ..spaceList = const [
        Space(id: 'work-1', name: 'Work', live: ['pb-a']),
      ];
    await _open(tester, fake);
    fake.emit(DeviceAdded(_stranger('pb-stranger', 'somebody-else')));
    await tester.pumpAndSettle();

    await tester.tap(_addTo('work-1'));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('space-candidate-pb-b')), findsOneWidget);
    expect(
      find.byKey(const Key('space-candidate-pb-stranger')),
      findsNothing,
      reason: 'a Space cannot hold a device this machine has not trusted',
    );
    expect(
      find.byKey(const Key('space-candidate-pb-a')),
      findsNothing,
      reason: 'it is already in the Space, with a row on screen saying so',
    );
    expect(_copy(tester), contains('Only devices this one already trusts'));
    expect(
      _copy(tester),
      contains('grants it nothing'),
      reason:
          'a list of devices under a name you typed would otherwise read '
          'like an access list',
    );
    // The sheet is where group language would creep in first — "invite" is the
    // word every other app uses for this gesture, and there is nothing here to
    // be invited to.
    final copy = _copy(tester).toLowerCase();
    for (final forbidden in ['invite', 'join', 'member', 'group', 'room']) {
      expect(
        copy,
        isNot(contains(forbidden)),
        reason:
            '"$forbidden" implies a '
            'shared roster; the chosen device is never told anything',
      );
    }
  });

  /// A stale device is excluded from the picker as firmly as a live one: it is
  /// in the Space, and re-offering it would either do nothing or be refused for
  /// the very reason its row is marked.
  testWidgets('a stale device counts as already in it', (tester) async {
    final fake = FakePeerBeam()
      ..trusted = [_pin('pb-a', 'laptop'), _pin('pb-b', 'desktop')]
      ..spaceList = const [
        Space(id: 'work-1', name: 'Work', live: ['pb-a'], stale: ['pb-b']),
      ];
    await _open(tester, fake);

    await tester.tap(_addTo('work-1'));
    await tester.pumpAndSettle();

    expect(find.text('Every trusted device is already in it'), findsOneWidget);
  });

  /// Nothing trusted at all is a different problem with different advice, and
  /// giving the wrong one sends someone to pair a device they already paired.
  testWidgets('with nothing trusted the picker says to pair first', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..spaceList = const [Space(id: 'work-1', name: 'Work')];
    await _open(tester, fake);

    await tester.tap(_addTo('work-1'));
    await tester.pumpAndSettle();

    expect(find.text('No trusted devices yet'), findsOneWidget);
    expect(_copy(tester), contains('Pair with a device first'));
  });

  testWidgets('choosing a device adds it and its row appears', (tester) async {
    final fake = FakePeerBeam()
      ..trusted = [_pin('pb-b', 'desktop')]
      ..spaceList = const [Space(id: 'work-1', name: 'Work')];
    await _open(tester, fake);

    await tester.tap(_addTo('work-1'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('space-candidate-pb-b')));
    await tester.pumpAndSettle();

    expect(fake.calls, contains('addSpaceMember:work-1:pb-b'));
    // Re-read rather than assumed: whether the engine calls the device live is
    // the engine's answer, and the row is where that answer shows up.
    expect(find.byKey(const Key('space-work-1-device-pb-b')), findsOneWidget);
    expect(find.text('1 device'), findsOneWidget);
  });

  /// **Deleting is undone, never confirmed.** The undo restores the name and
  /// then every device the card was showing.
  testWidgets('deleting a Space offers an undo instead of a prompt', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..trusted = [_pin('pb-a', 'laptop')]
      ..spaceList = const [
        Space(id: 'work-1', name: 'Work', live: ['pb-a'], stale: ['pb-ghost']),
      ];
    await _open(tester, fake);

    await _menu(tester, 'Delete Space');

    expect(
      find.byType(AlertDialog),
      findsNothing,
      reason: 'a delete this cheap to reverse must not train dialog-dismissal',
    );
    expect(fake.calls, contains('deleteSpace:work-1'));
    expect(find.byKey(const Key('space-work-1')), findsNothing);
    expect(find.text('Deleted “Work”'), findsOneWidget);

    await tester.tap(find.text('Undo'));
    await tester.pumpAndSettle();

    expect(fake.calls, contains('createSpace:Work'));
    expect(
      fake.calls.where((c) => c.startsWith('addSpaceMember:')).length,
      2,
      reason:
          'every device on the card is attempted, the stale one included: it '
          'may have been re-paired since, and only the engine knows',
    );
    expect(find.text('Work'), findsOneWidget);
    expect(find.text('laptop'), findsOneWidget);
  });

  /// A refused write leaves the user looking at what is actually stored, with
  /// the reason — never at a row the engine rejected.
  testWidgets('a refused create adds no card and says so', (tester) async {
    final fake = FakePeerBeam()..failing.add('createSpace');
    await _open(tester, fake);

    await tester.tap(find.byType(FloatingActionButton));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField), 'Work');
    // Settled before tapping: the Create button is disabled until the field
    // holds something, and tapping it a frame early taps nothing at all.
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, 'Create'));
    await tester.pumpAndSettle();

    expect(_copy(tester), contains('Could not create “Work”'));
    expect(find.byKey(const Key('space-sp0')), findsNothing);
    expect(find.text('No Spaces yet'), findsOneWidget);
  });

  /// The dialog checks only what it can see for itself — that there is
  /// something to submit, and that a rename is a change. The name rules
  /// themselves belong to the engine, which refuses with a reason.
  testWidgets('renaming will not submit an unchanged name', (tester) async {
    final fake = FakePeerBeam()
      ..spaceList = const [Space(id: 'work-1', name: 'Work')];
    await _open(tester, fake);

    await _menu(tester, 'Rename');
    final button = tester.widget<FilledButton>(
      find.widgetWithText(FilledButton, 'Rename'),
    );
    expect(button.onPressed, isNull);

    await tester.enterText(find.byType(TextField), 'Work laptops');
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, 'Rename'));
    await tester.pumpAndSettle();

    expect(fake.calls, contains('renameSpace:work-1:Work laptops'));
    expect(find.text('Work laptops'), findsOneWidget);
  });

  /// Taking out a live device is reversible in one write, so it comes with an
  /// undo.
  testWidgets('removing a live device offers an undo that puts it back', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..trusted = [_pin('pb-a', 'laptop')]
      ..spaceList = const [
        Space(id: 'work-1', name: 'Work', live: ['pb-a']),
      ];
    await _open(tester, fake);

    await tester.tap(_takeOut('work-1', 'pb-a'));
    await tester.pumpAndSettle();

    expect(fake.calls, contains('removeSpaceMember:work-1:pb-a'));
    expect(find.text('Took laptop out of “Work”'), findsOneWidget);

    await tester.tap(find.text('Undo'));
    await tester.pumpAndSettle();

    expect(fake.calls, contains('addSpaceMember:work-1:pb-a'));
    expect(find.byKey(const Key('space-work-1-device-pb-a')), findsOneWidget);
  });

  /// **Taking out a stale device works, and is not offered an undo.** The
  /// engine will not add a device it holds no live pin for, so an Undo here
  /// would fail the moment it was pressed. It says what would make the device
  /// addable again instead.
  testWidgets('removing a stale device explains why there is no undo', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..spaceList = const [
        Space(id: 'work-1', name: 'Work', stale: ['pb-gone']),
      ];
    await _open(tester, fake);

    await tester.tap(_takeOut('work-1', 'pb-gone'));
    await tester.pumpAndSettle();

    expect(fake.calls, contains('removeSpaceMember:work-1:pb-gone'));
    expect(_copy(tester), contains('once this device trusts it again'));
    expect(
      find.text('Undo'),
      findsNothing,
      reason: 'an undo that cannot work is worse than none',
    );
    expect(find.byKey(const Key('space-work-1-device-pb-gone')), findsNothing);
  });
}
