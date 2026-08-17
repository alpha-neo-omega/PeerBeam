// The Trusted devices list must not render a stranger like a chosen device.
//
// PeerBeam's handshake pins every never-seen peer as it connects, so a later
// key change is detectable — which means this list contains devices nobody
// approved. Only an *approved* device is sent presence, clipboard contents, or
// an accepted pipe, so a screen that shows both under the same shield is
// telling the user a stranger is trusted.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/features/settings/settings_screen.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

TrustedDevice _device({
  required String id,
  required String name,
  required bool approved,
}) => TrustedDevice(
  id: id,
  name: name,
  fingerprint: 'ab12cd34ef56ab12cd34ef56ab12cd34',
  trustedAt: DateTime(2026, 8, 18),
  approved: approved,
);

/// Open Settings with the fake's trust list loaded, and scroll the Trusted
/// devices section into view.
///
/// Both steps are needed and neither is incidental: the repository is filled by
/// an explicit refresh (it deliberately does not fetch in its constructor, since
/// the engine may not be initialised yet at construction time), and the section
/// sits far enough down a lazy scroll view that it is not built until reached.
Future<AppState> _open(
  WidgetTester tester,
  FakePeerBeam fake, {
  required String scrollTo,
}) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: const MaterialApp(home: SettingsScreen()),
    ),
  );
  await tester.pump();
  await state.trust.refresh();
  await tester.pump();
  await tester.scrollUntilVisible(
    find.text(scrollTo),
    300,
    scrollable: find.byType(Scrollable).first,
  );
  await tester.pumpAndSettle();
  return state;
}

/// Whether the tile titled [deviceName] carries [icon].
///
/// Scoped deliberately: the shield icon appears elsewhere on the Settings
/// screen, so an unscoped `find.byIcon` would happily pass on a completely
/// different widget and prove nothing about this device's row.
bool _iconIn(WidgetTester tester, String deviceName, IconData icon) => find
    .descendant(
      of: find.widgetWithText(ListTile, deviceName),
      matching: find.byIcon(icon),
    )
    .evaluate()
    .isNotEmpty;

void main() {
  testWidgets('a pinned-but-unapproved device is marked, not shown as trusted', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..trusted = [
        _device(id: 'pb-mine', name: 'My Laptop', approved: true),
        _device(id: 'pb-stranger', name: 'Unknown Box', approved: false),
      ];
    await _open(tester, fake, scrollTo: 'Unknown Box');

    // The stranger is called out in words, not only by an icon — an icon alone
    // is not something a screen reader or a colour-blind user can rely on.
    expect(find.textContaining('not approved'), findsOneWidget);
    // ...and only for the stranger.
    expect(find.text('My Laptop'), findsOneWidget);
    expect(find.text('Unknown Box'), findsOneWidget);
    // Scoped to each device's own tile: the shield icon is used elsewhere on
    // this screen, so an unscoped finder would pass on the wrong widget.
    expect(_iconIn(tester, 'Unknown Box', Icons.help_outline_rounded), isTrue);
    expect(_iconIn(tester, 'My Laptop', Icons.verified_user_rounded), isTrue);
    expect(_iconIn(tester, 'My Laptop', Icons.help_outline_rounded), isFalse);
  });

  testWidgets('an all-approved list says nothing about approval', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..trusted = [_device(id: 'pb-mine', name: 'My Laptop', approved: true)];
    await _open(tester, fake, scrollTo: 'My Laptop');

    expect(find.textContaining('not approved'), findsNothing);
    expect(_iconIn(tester, 'My Laptop', Icons.help_outline_rounded), isFalse);
    expect(_iconIn(tester, 'My Laptop', Icons.verified_user_rounded), isTrue);
  });

  test('an engine that predates the field is read as NOT approved', () {
    // Unknown is not approval: an older engine omits the key entirely, and
    // defaulting that to true would silently re-open the hole this closes.
    final d = TrustedDevice.fromJson(const {
      'id': 'pb-old',
      'name': 'Old Engine',
      'fingerprint': 'ff',
      'trusted_at': '2026-08-18T00:00:00Z',
    });
    expect(d.approved, isFalse);
  });
}
