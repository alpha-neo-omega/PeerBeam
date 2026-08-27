// The Trusted devices list must not render a stranger like a chosen device.
//
// PeerBeam's handshake pins every never-seen peer as it connects, so a later
// key change is detectable — which means this list contains devices nobody
// approved. Only an *approved* device is sent presence, clipboard contents, or
// an accepted pipe, so a screen that shows both under the same shield is
// telling the user a stranger is trusted.

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
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
  Set<String>? permissions,
}) => TrustedDevice(
  id: id,
  name: name,
  fingerprint: 'ab12cd34ef56ab12cd34ef56ab12cd34',
  trustedAt: DateTime(2026, 8, 18),
  approved: approved,
  // An approved device the engine has not narrowed holds everything, which is
  // what `trust approve` grants; an unapproved one holds nothing, because the
  // engine reports the *effective* set and permissions never create a standing.
  permissions:
      permissions ??
      (approved ? PeerBeamPermission.all.toSet() : const <String>{}),
);

/// The switch inside [deviceId]'s permission block whose title is [label].
///
/// Scoped by the block's key rather than by the device's name: every approved
/// device renders the same five labels, so anything looser matches the wrong row
/// as soon as a test has two devices — which the interesting ones all do.
Finder _switchFor(String deviceId, String label) => find.descendant(
  of: find.byKey(Key('device-permissions-$deviceId')),
  matching: find.widgetWithText(SwitchListTile, label),
);

/// Open [deviceId]'s permission block.
///
/// The switches collapse by default: seven of them per device, each with a
/// sentence under it, is about a screenful per approved device, and the trusted
/// list stopped being a list of devices at more than one or two. Every test
/// that asserts on a switch opens the block first, which is also the user's
/// path to it.
Future<void> _expandPermissions(WidgetTester tester, String deviceId) async {
  final header = find.descendant(
    of: find.byKey(Key('device-permissions-$deviceId')),
    matching: find.byType(InkWell),
  );
  expect(
    header,
    findsOneWidget,
    reason: 'collapsed, so the header is the only InkWell in the block',
  );
  // The block may be scrolled out of view: a test that narrows one device
  // scrolls to a *different* one, and the header is then in the tree but off
  // screen, where a tap lands on nothing.
  await tester.ensureVisible(header);
  await tester.pumpAndSettle();
  await tester.tap(header);
  await tester.pumpAndSettle();
}

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

/// Tap a switch that may be scrolled out of the viewport.
///
/// The permission block sits under a device row inside a long lazy scroll view,
/// so with more than one device a switch is routinely outside the 800x600 test
/// surface even though it is built. Scrolling it in is part of tapping it here,
/// not something a test should be able to forget.
Future<void> _flip(WidgetTester tester, Finder tile) async {
  await tester.ensureVisible(tile);
  await tester.pumpAndSettle();
  await tester.tap(tile);
  await tester.pumpAndSettle();
}

/// A permission name this build does not know, for the forward-compatibility
/// test. Guarded by an assertion at the point of use so it cannot quietly
/// become real.
const unknownPermission = 'xyzzy-not-a-permission';

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

  /// **A fingerprint that cannot leave the screen cannot do its job.** It is
  /// shown shortened so the row stays readable, and comparing 16 hex characters
  /// against another device proves less than it looks like it does — so the copy
  /// has to carry the whole value, not what is rendered.
  testWidgets('tapping a fingerprint copies the whole thing, not the short '
      'form', (tester) async {
    final copied = <String>[];
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      SystemChannels.platform,
      (call) async {
        if (call.method == 'Clipboard.setData') {
          copied.add((call.arguments as Map)['text'] as String);
        }
        return null;
      },
    );
    addTearDown(
      () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        SystemChannels.platform,
        null,
      ),
    );

    final fake = FakePeerBeam()
      ..trusted = [_device(id: 'pb-mine', name: 'My Laptop', approved: true)];
    await _open(tester, fake, scrollTo: 'My Laptop');

    await tester.tap(find.byTooltip('Tap to copy the full fingerprint').first);
    await tester.pumpAndSettle();

    expect(copied, ['ab12cd34ef56ab12cd34ef56ab12cd34']);
    expect(find.text('Full fingerprint copied'), findsOneWidget);
  });

  /// **A revoke that did not happen looked exactly like one that did.** The
  /// call swallowed its error and returned nothing, so the dialog closed, the
  /// device stayed trusted, and the user was left believing they had withdrawn
  /// it. Of everything on this screen, that is the one failure that must not be
  /// quiet.
  testWidgets('a revoke that fails says so instead of looking done', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..trusted = [_device(id: 'pb-mine', name: 'My Laptop', approved: true)]
      ..trustRemoveError = StateError('engine said no');
    await _open(tester, fake, scrollTo: 'My Laptop');

    await tester.tap(find.byTooltip('Revoke trust').first);
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, 'Revoke'));
    await tester.pumpAndSettle();

    expect(
      find.textContaining('was not revoked'),
      findsOneWidget,
      reason: 'the failure was swallowed and the device looks revoked',
    );
    // And it is still there, which is the truth of it.
    expect(find.text('My Laptop'), findsOneWidget);
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

  testWidgets('an approved device shows one switch per permission, on', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..trusted = [_device(id: 'pb-mine', name: 'My Laptop', approved: true)];
    await _open(tester, fake, scrollTo: 'My Laptop');
    await _expandPermissions(tester, 'pb-mine');

    for (final permission in PeerBeamPermission.all) {
      final label = PeerBeamPermission.label(permission);
      final tile = _switchFor('pb-mine', label);
      expect(tile, findsOneWidget, reason: 'no switch for $label');
      expect(
        tester.widget<SwitchListTile>(tile).value,
        isTrue,
        reason: '$label is granted, so its switch must read on',
      );
      // Each switch says what it allows. A security control whose consequence
      // is unstated is one people flip to find out.
      expect(
        find.text(PeerBeamPermission.description(permission)),
        findsOneWidget,
      );
    }
  });

  testWidgets('a narrowed permission reads off while the others read on', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..trusted = [
        _device(
          id: 'pb-mine',
          name: 'My Laptop',
          approved: true,
          permissions: PeerBeamPermission.all
              .where((p) => p != PeerBeamPermission.clipboard)
              .toSet(),
        ),
      ];
    await _open(tester, fake, scrollTo: 'My Laptop');
    await _expandPermissions(tester, 'pb-mine');

    expect(
      tester.widget<SwitchListTile>(_switchFor('pb-mine', 'Clipboard')).value,
      isFalse,
      reason: 'the revoked permission must read off',
    );
    for (final permission in PeerBeamPermission.all.where(
      (p) => p != PeerBeamPermission.clipboard,
    )) {
      expect(
        tester
            .widget<SwitchListTile>(
              _switchFor('pb-mine', PeerBeamPermission.label(permission)),
            )
            .value,
        isTrue,
        reason: 'revoking clipboard must not disturb $permission',
      );
    }
  });

  testWidgets('toggling a switch asks the engine for that one permission', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..trusted = [
        _device(id: 'pb-mine', name: 'My Laptop', approved: true),
        _device(id: 'pb-other', name: 'Desktop', approved: true),
      ];
    await _open(tester, fake, scrollTo: 'Desktop');
    // Both: the assertion at the end is that flipping one device's switch left
    // the other's alone, and a collapsed block has no switch to check.
    await _expandPermissions(tester, 'pb-mine');
    await _expandPermissions(tester, 'pb-other');

    await _flip(tester, _switchFor('pb-mine', 'Clipboard'));

    expect(fake.permissionCalls, hasLength(1));
    // The device it belongs to, the permission it names, and the direction the
    // switch was moved — a call that got any of the three wrong would silently
    // change the wrong thing.
    expect(fake.permissionCalls.single.id, 'pb-mine');
    expect(fake.permissionCalls.single.permission, 'clipboard');
    expect(fake.permissionCalls.single.granted, isFalse);

    // And the switch reflects it without waiting for a refetch.
    expect(
      tester.widget<SwitchListTile>(_switchFor('pb-mine', 'Clipboard')).value,
      isFalse,
    );
    expect(
      tester.widget<SwitchListTile>(_switchFor('pb-other', 'Clipboard')).value,
      isTrue,
      reason: 'the other device must be untouched',
    );
  });

  testWidgets('a refused toggle snaps back to what the engine actually holds', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..trusted = [_device(id: 'pb-mine', name: 'My Laptop', approved: true)]
      ..trustSetPermissionError = Exception('unknown permission');
    await _open(tester, fake, scrollTo: 'My Laptop');
    await _expandPermissions(tester, 'pb-mine');

    await _flip(tester, _switchFor('pb-mine', 'Pipes'));

    expect(fake.permissionCalls, hasLength(1), reason: 'it was attempted');
    expect(
      tester.widget<SwitchListTile>(_switchFor('pb-mine', 'Pipes')).value,
      isTrue,
      reason:
          'a switch must never be left showing something that did not happen',
    );
  });

  testWidgets('permissions start collapsed, so a device row stays one row', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..trusted = [_device(id: 'pb-mine', name: 'My Laptop', approved: true)];
    await _open(tester, fake, scrollTo: 'My Laptop');

    // Collapsed is the point: seven switches per device is what made the list
    // unreadable. Not one of them is built until the header is tapped.
    for (final permission in PeerBeamPermission.all) {
      expect(
        _switchFor('pb-mine', PeerBeamPermission.label(permission)),
        findsNothing,
        reason: '${PeerBeamPermission.label(permission)} must start hidden',
      );
    }

    await _expandPermissions(tester, 'pb-mine');

    for (final permission in PeerBeamPermission.all) {
      expect(
        _switchFor('pb-mine', PeerBeamPermission.label(permission)),
        findsOneWidget,
        reason: '${PeerBeamPermission.label(permission)} must appear on expand',
      );
    }
  });

  testWidgets('a collapsed row still says how many permissions are allowed', (
    tester,
  ) async {
    final total = PeerBeamPermission.all.length;
    final fake = FakePeerBeam()
      ..trusted = [
        _device(id: 'pb-mine', name: 'My Laptop', approved: true),
        _device(
          id: 'pb-other',
          name: 'Desktop',
          approved: true,
          // Narrowed: browse and notes withheld.
          permissions: {'files', 'chat', 'clipboard'},
        ),
      ];
    await _open(tester, fake, scrollTo: 'Desktop');

    // The security-relevant fact — this device is not on the defaults — has to
    // survive collapsing, or hiding the switches would hide the answer too.
    expect(find.textContaining('all $total allowed'), findsOneWidget);
    expect(find.textContaining('3 of $total allowed'), findsOneWidget);
  });

  testWidgets('a pinned device can be trusted without receiving a file', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..trusted = [
        _device(id: 'pb-seen', name: 'Unknown Box', approved: false),
      ];
    final state = await _open(tester, fake, scrollTo: 'Unknown Box');

    // The row used to say "Accept a transfer from it to approve" and offer
    // nothing — the engine and the CLI could both approve, the GUI could not.
    await tester.tap(find.text('Trust'));
    await tester.pumpAndSettle();

    expect(fake.approveCalls, hasLength(1));
    expect(fake.approveCalls.single.id, 'pb-seen');
    expect(fake.approveCalls.single.share, isTrue);
    expect(
      state.trust.items.single.approved,
      isTrue,
      reason: 'the row must re-read as approved, not merely claim it',
    );
  });

  testWidgets('trust-without-sharing approves and grants nothing', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..trusted = [
        _device(id: 'pb-seen', name: 'Unknown Box', approved: false),
      ];
    final state = await _open(tester, fake, scrollTo: 'Unknown Box');

    await tester.tap(find.text('Trust, share nothing'));
    await tester.pumpAndSettle();

    expect(fake.approveCalls.single.share, isFalse);
    final device = state.trust.items.single;
    expect(device.approved, isTrue, reason: 'it is no longer a stranger');
    for (final permission in PeerBeamPermission.all) {
      expect(
        device.may(permission),
        isFalse,
        reason: 'trust-without-sharing granted $permission',
      );
    }
  });

  testWidgets('a device that never connected is told, not claimed as trusted', (
    tester,
  ) async {
    // Pinned row on screen, but the engine answers `pinned: false` — the store
    // lost it, or a concurrent revoke landed first.
    final fake = FakePeerBeam()
      ..trusted = [
        _device(id: 'pb-seen', name: 'Unknown Box', approved: false),
      ];
    await _open(tester, fake, scrollTo: 'Unknown Box');
    fake.trusted = const [];

    await tester.tap(find.text('Trust'));
    await tester.pumpAndSettle();

    // It must say why rather than showing a device as trusted while the store
    // holds nothing for it.
    expect(find.textContaining('never connected'), findsOneWidget);
  });

  testWidgets('an approved device is offered no approve buttons', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..trusted = [_device(id: 'pb-mine', name: 'My Laptop', approved: true)];
    await _open(tester, fake, scrollTo: 'My Laptop');

    expect(find.byKey(const Key('approve-pb-mine')), findsNothing);
    expect(find.text('Trust, share nothing'), findsNothing);
  });

  testWidgets('a pinned-but-unapproved device is offered no permissions', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..trusted = [
        _device(id: 'pb-stranger', name: 'Unknown Box', approved: false),
      ];
    await _open(tester, fake, scrollTo: 'Unknown Box');

    // Permissions narrow a standing and never create one, so there is nothing
    // to narrow here — and offering a switch that grants nothing would be a
    // control with no effect.
    expect(find.text('What Unknown Box may do'), findsNothing);
    // Asserted by description rather than by `find.byType(SwitchListTile)`:
    // this screen has plenty of unrelated switches, so the broad finder would
    // pass or fail on whichever of them happens to be in the viewport.
    for (final permission in PeerBeamPermission.all) {
      expect(
        find.text(PeerBeamPermission.description(permission)),
        findsNothing,
        reason: 'no $permission switch for a device that may nothing',
      );
    }
  });

  test('permissions decode as an explicit set, and default to empty', () {
    final narrowed = TrustedDevice.fromJson(const {
      'id': 'pb-a',
      'name': 'Laptop',
      'fingerprint': 'ff',
      'trusted_at': '2026-08-18T00:00:00Z',
      'approved': true,
      'permissions': ['files', 'chat'],
    });
    expect(narrowed.permissions, {'files', 'chat'});
    expect(narrowed.may(PeerBeamPermission.files), isTrue);
    expect(narrowed.may(PeerBeamPermission.clipboard), isFalse);

    // An engine that predates the field grants nothing here: a permission this
    // app cannot see is one it must not claim is granted.
    final old = TrustedDevice.fromJson(const {
      'id': 'pb-old',
      'name': 'Old Engine',
      'fingerprint': 'ff',
      'trusted_at': '2026-08-18T00:00:00Z',
      'approved': true,
    });
    expect(old.permissions, isEmpty);
    expect(old.may(PeerBeamPermission.files), isFalse);
  });

  test('a permission name this build has no label for is not offered', () {
    // Forward compatibility: a newer engine may report a permission this build
    // cannot render. It decodes and is readable, but only the names in
    // `PeerBeamPermission.all` get a switch — a control this build cannot
    // describe is worse than none.
    // **Proved unknown, not assumed.** This test's placeholder was 'browse'
    // until shared folders shipped, at which point it started honouring the
    // grant it exists to reject — and said nothing. The assertion comes first
    // now, so a fourth promotion fails loudly instead of passing vacuously.
    expect(
      PeerBeamPermission.all,
      isNot(contains(unknownPermission)),
      reason:
          '$unknownPermission is a real permission now — this test needs a '
          'different placeholder, and would otherwise check nothing',
    );
    final future = TrustedDevice.fromJson(const {
      'id': 'pb-a',
      'name': 'Laptop',
      'fingerprint': 'ff',
      'trusted_at': '2026-08-18T00:00:00Z',
      'approved': true,
      'permissions': ['files', unknownPermission],
    });
    expect(future.may(unknownPermission), isTrue, reason: 'it decodes');
  });

  testWidgets('About reports the engine version, not a number written in Dart', (
    tester,
  ) async {
    // This screen said "Version 0.3.0" for three releases, under a comment
    // asking whoever bumped the version to keep it in sync. A hand-maintained
    // duplicate of a fact the engine already knows is a claim nobody re-checks,
    // so the test asserts the value came from the engine by giving the engine
    // an unmistakable one.
    final fake = FakePeerBeam()..engineVersionValue = '4.5.6';
    await _open(tester, fake, scrollTo: 'PeerBeam');

    expect(find.text('Version 4.5.6 · AGPL-3.0'), findsOneWidget);
  });

  testWidgets('About admits an unknown version rather than inventing one', (
    tester,
  ) async {
    final fake = FakePeerBeam()..engineVersionValue = null;
    await _open(tester, fake, scrollTo: 'PeerBeam');

    expect(find.text('Version unknown · AGPL-3.0'), findsOneWidget);
  });

  /// **The auto-accept switch must not promise more than the engine does.**
  ///
  /// Its subtitle read "Skip the prompt for pinned devices", and pinning is not
  /// the bar: the handshake pins every stranger that reaches this machine, while
  /// `admit_transfer` on the engine side auto-accepts only a device that is
  /// *approved* and still holds the `files` permission. A user who read the old
  /// line and left the switch on believed it covered devices that in fact still
  /// prompt — the one direction this copy must never be wrong in, since the
  /// whole point of the setting is knowing when nobody will be asked.
  testWidgets('the auto-accept switch says approved, not pinned', (
    tester,
  ) async {
    await _open(
      tester,
      FakePeerBeam(),
      scrollTo: 'Auto-accept trusted devices',
    );

    final subtitle = find.descendant(
      of: find.widgetWithText(SwitchListTile, 'Auto-accept trusted devices'),
      matching: find.byType(Text),
    );
    final copy = tester
        .widgetList<Text>(subtitle)
        .map((t) => t.data ?? '')
        .join(' ');

    expect(
      copy,
      contains('approved'),
      reason: 'approval is the state the engine actually requires',
    );
    expect(
      copy,
      isNot(contains('pinned')),
      reason: 'a pinned-but-unapproved device is prompted for either way',
    );
  });
}
