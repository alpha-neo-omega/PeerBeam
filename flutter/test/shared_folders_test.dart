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
//  4. **Android is told, not offered a picker.** A share is a path the engine
//     canonicalises and reads; Android reaches a folder only through a grant,
//     and this app asks for no storage permission that would make a bare path
//     work instead. The picker was shown there anyway, and whichever way it
//     ended — a folder written that served nothing, or a pick that threw —
//     nothing on screen said why.

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/settings/settings_screen.dart';
import 'package:peerbeam/features/settings/shared_folders_card.dart';
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

/// Run a widget test with the target platform pinned to [platform].
///
/// The reset is a synchronous `finally` rather than an `addTearDown`:
/// `testWidgets` re-checks foundation's debug variables the instant its
/// callback returns, before the tear-down queue unwinds, so a tear-down reset
/// lands too late and trips that check on the *next* test.
void _on(
  TargetPlatform platform,
  String description,
  Future<void> Function(WidgetTester tester) body,
) {
  testWidgets(description, (tester) async {
    debugDefaultTargetPlatformOverride = platform;
    try {
      await body(tester);
    } finally {
      debugDefaultTargetPlatformOverride = null;
    }
  });
}

/// A test of the editor, which exists on desktop only.
///
/// The pin is load-bearing, not decoration: flutter_test reports
/// `TargetPlatform.android` unless told otherwise, and Android is the one
/// platform where this card draws no editor at all.
void _onDesktop(
  String description,
  Future<void> Function(WidgetTester tester) body,
) => _on(TargetPlatform.linux, description, body);

/// A test of what Android is given instead of the editor.
void _onAndroid(
  String description,
  Future<void> Function(WidgetTester tester) body,
) => _on(TargetPlatform.android, description, body);

void main() {
  /// **Empty must read as a choice.** Nothing is shared until someone shares
  /// it, and a section that rendered an empty card would let that safe default
  /// look like a screen that failed to load.
  _onDesktop('nothing shared reads as the deliberate default', (tester) async {
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
  _onDesktop('a broken share is listed and marked, never dropped', (
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
  _onDesktop('two shares with one name are told apart by their paths', (
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
  _onDesktop('removing a share reaches the engine at once', (tester) async {
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
  _onDesktop('the section states that changes take effect immediately', (
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
  _onDesktop('un-sharing everything returns to "Nothing shared"', (
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

  /// **A control that cannot work is explained, not offered.** The engine
  /// builds a share by canonicalising a path and reading it; Android hands an
  /// app a folder as a grant instead, and this app holds no storage permission
  /// that would make a bare path work. So the pick could not lead anywhere,
  /// and offering it spent the user's attention on nothing while leaving them
  /// to believe a folder was on offer.
  ///
  /// The same shape `_SaveRulesCard` gives auto-save rules on Android — one
  /// explanation, no control — because it is the same situation.
  group('a platform that cannot serve a folder', () {
    /// The card alone, not the whole of Settings: pinning the platform to
    /// Android also swaps in the screen's SAF-backed "Save to" row and its
    /// background section, whose platform channels answer nothing in a widget
    /// test. The card is what is on trial here.
    Future<void> pump(WidgetTester tester) async {
      final state = AppState.live(FakePeerBeam());
      addTearDown(state.dispose);
      await tester.pumpWidget(
        AppScope(
          state: state,
          child: const MaterialApp(home: Scaffold(body: SharedFoldersCard())),
        ),
      );
      await tester.pumpAndSettle();
    }

    _onAndroid('Android is told why, and given no picker', (tester) async {
      await pump(tester);

      expect(
        find.text('Share a folder'),
        findsNothing,
        reason: 'a pick that can never be served must not be offered',
      );
      expect(find.text('Not available on this device'), findsOneWidget);
      final copy = tester
          .widgetList<Text>(find.byType(Text))
          .map((t) => t.data ?? '')
          .join(' ');
      expect(
        copy,
        contains('never as a path'),
        reason: 'the limit has to be explained, not merely applied',
      );
    });

    /// And the gate is narrow: everywhere a share can actually be served, the
    /// picker is exactly where it was.
    _onDesktop('desktop still offers the picker', (tester) async {
      await pump(tester);

      expect(find.text('Share a folder'), findsOneWidget);
      expect(find.text('Not available on this device'), findsNothing);
    });
  });
}
