// The Shared folders section: what this device offers to peers who may browse.
//
// Three properties carry the feature, and each of them is a way of being wrong
// that this section exists to prevent:
//
//  1. **Empty is the default, not a failure.** Nothing is shared until someone
//     chooses it, so an empty list has to read as deliberate. A blank card and
//     a load that failed look identical.
//  2. **A folder that has gone is still listed.** Hiding it would leave someone
//     believing they share something they do not — the single worst belief this
//     screen could create.
//  3. **The path is shown.** Two folders called `Documents` are
//     indistinguishable by name, and un-sharing the wrong one is unrecoverable
//     in the only sense that matters: you do not know it happened.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/settings/settings_screen.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

/// Open Settings and scroll the Shared folders section into view.
///
/// The settings document is loaded explicitly for the same reason the save-rule
/// tests load it: the store is built before the engine is known, so nothing is
/// fetched in its constructor. The section's own list comes from the API, not
/// the store, and arrives on the first post-frame callback.
Future<AppState> _open(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: const MaterialApp(home: SettingsScreen()),
    ),
  );
  await tester.pump();
  await state.settings.load(fake);
  await tester.pumpAndSettle();
  await tester.scrollUntilVisible(
    find.text('SHARED FOLDERS'),
    300,
    scrollable: find.byType(Scrollable).first,
  );
  await tester.pumpAndSettle();
  return state;
}

/// The tooltip'd remove button belonging to one share — never `find.byTooltip`
/// alone, which matches every row the moment there are two.
Finder _stopSharing(String path) => find.descendant(
  of: find.byKey(Key('shared-folder-$path')),
  matching: find.byTooltip('Stop sharing'),
);

void main() {
  /// **Empty must read as a choice.** Nothing is shared until someone shares
  /// it, and a section that rendered an empty card would let that safe default
  /// look like a screen that failed to load.
  testWidgets('nothing shared reads as the deliberate default', (tester) async {
    await _open(tester, FakePeerBeam());

    expect(find.text('Nothing shared'), findsOneWidget);
    final copy = tester
        .widgetList<Text>(find.byType(Text))
        .map((t) => t.data ?? '')
        .join(' ');
    expect(
      copy,
      contains('until you choose one'),
      reason: 'empty has to be stated as the default, not merely displayed',
    );
    expect(copy, contains('This is the default, not a problem'));
  });

  /// **A share whose folder is gone is visible, not hidden.** Dropping it would
  /// leave someone believing they share something they do not.
  testWidgets('a broken share is listed and marked, never dropped', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..shares = const [
        SharedFolder(name: 'Photos', path: '/home/me/Photos', exists: true),
        SharedFolder(name: 'Old', path: '/mnt/usb/Old', exists: false),
      ];
    await _open(tester, fake);

    expect(find.text('/mnt/usb/Old'), findsOneWidget);
    expect(
      find.textContaining('This folder is gone'),
      findsOneWidget,
      reason: 'a share that offers nothing must say so on its own row',
    );
    // The working one is untouched by its broken neighbour.
    expect(find.text('/home/me/Photos'), findsOneWidget);
  });

  /// **The path, not just the name.** Two shares can carry the same name, and
  /// the name alone cannot tell them apart.
  testWidgets('two shares with one name are told apart by their paths', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..shares = const [
        SharedFolder(
          name: 'Documents',
          path: '/home/me/work/Documents',
          exists: true,
        ),
        SharedFolder(
          name: 'Documents',
          path: '/home/me/archive/Documents',
          exists: true,
        ),
      ];
    await _open(tester, fake);

    expect(find.text('Documents'), findsNWidgets(2));
    expect(find.text('/home/me/work/Documents'), findsOneWidget);
    expect(find.text('/home/me/archive/Documents'), findsOneWidget);
  });

  /// **Un-sharing is immediate.** There is no Save button, so the write has to
  /// reach the engine on the tap — a folder that stays shared until the next
  /// restart is the one direction this must never lag in.
  testWidgets('removing a share reaches the engine at once', (tester) async {
    final fake = FakePeerBeam()
      ..shares = const [
        SharedFolder(name: 'Photos', path: '/home/me/Photos', exists: true),
        SharedFolder(name: 'Media', path: '/srv/media', exists: true),
      ];
    await _open(tester, fake);

    await tester.tap(_stopSharing('/srv/media'));
    await tester.pumpAndSettle();

    expect(
      fake.calls,
      contains('setSharedFolders:/home/me/Photos'),
      reason:
          'the engine must be sent the remaining list, not told to drop one',
    );
    expect(find.text('/srv/media'), findsNothing);
    expect(find.text('/home/me/Photos'), findsOneWidget);
  });

  /// And the copy says so, because nothing else on the card could: there is no
  /// Save button to imply otherwise and no confirmation to imply delay.
  testWidgets('the section states that changes take effect immediately', (
    tester,
  ) async {
    await _open(tester, FakePeerBeam());

    final copy = tester
        .widgetList<Text>(find.byType(Text))
        .map((t) => t.data ?? '')
        .join(' ');
    expect(copy, contains('take effect immediately'));
    expect(
      copy,
      contains('granted Browse'),
      reason: 'who can see a shared folder is the point of sharing it',
    );
  });

  /// Un-sharing the last folder returns the section to its empty state — the
  /// same words as a device that never shared anything, because that is now the
  /// same fact.
  testWidgets('un-sharing everything returns to "Nothing shared"', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..shares = const [
        SharedFolder(name: 'Photos', path: '/home/me/Photos', exists: true),
      ];
    await _open(tester, fake);
    expect(find.text('Nothing shared'), findsNothing);

    await tester.tap(_stopSharing('/home/me/Photos'));
    await tester.pumpAndSettle();

    expect(fake.calls, contains('setSharedFolders:'));
    expect(await fake.sharedFolders(), isEmpty);
    expect(find.text('Nothing shared'), findsOneWidget);
  });
}
