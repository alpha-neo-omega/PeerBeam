// The Spaces screen: what a Space is, and — mostly — what it is not.
//
// `docs/SPACES.md` spends half its length on the negative half of this feature,
// and these tests are that half made executable. Four ways this screen could
// lie, each of which one test exists to prevent:
//
//  1. **Implying a group.** A Space has no roster on the wire, no group id and
//     no membership messages: nobody is invited, nothing is joined, and two
//     devices in one Space cannot tell. Any word suggesting otherwise describes
//     a feature that does not exist and, being hub-shaped, cannot.
//  2. **Presenting a failed read as an empty list.** "No Spaces yet" is a claim
//     about the engine's answer. Said when no answer came back, it tells
//     someone their Spaces are gone.
//  3. **Showing a stale device as an ordinary one, or dropping it.** The first
//     is a lie about where a send goes; the second leaves someone wondering
//     whether they ever added the device.
//  4. **Saying "no devices" of a Space that holds three revoked ones.** That
//     sends the user off to add a device they added months ago, and hides the
//     thing that actually needs doing: re-pair, or take it out.

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'package:peerbeam/features/spaces/spaces_screen.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/staging.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

/// Open Spaces with the fake's list and trust store read.
///
/// The trust refresh is explicit for the reason `TrustRepository` documents: it
/// deliberately fetches nothing in its constructor, because during boot the
/// engine is not initialised yet. The screen renders device *names* out of it,
/// so a test that skipped this would be asserting against ids only.
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

/// Every word the screen is currently showing, as one string.
String _copy(WidgetTester tester) => tester
    .widgetList<Text>(find.byType(Text))
    .map((t) => t.data ?? '')
    .join(' ');

void main() {
  // Nothing on this screen persists through SharedPreferences, but the shared
  // `AppState.live` wiring does: unprimed, that channel never answers and the
  // test hangs instead of failing.
  setUp(() => SharedPreferences.setMockInitialValues({}));

  /// **A Space is local, and the screen has to keep saying so.** This is the
  /// one assertion that cannot be recovered by a later fix: once the copy
  /// implies a shared roster, every other screen inherits the belief.
  testWidgets(
    'the screen states that nothing about a Space leaves the device',
    (tester) async {
      await _open(tester, FakePeerBeam());

      final copy = _copy(tester);
      expect(copy, contains('Nothing about it leaves this machine'));
      expect(
        copy,
        contains('no device is told it is in one'),
        reason: 'the peers never learn a Space exists — that is the feature',
      );
      expect(copy, contains('no device can see the others'));
    },
  );

  /// The same claim from the other side: no group, room or invitation language
  /// anywhere, because there is no group, no room and nothing to be invited to.
  testWidgets('no wording suggests a group anyone has joined', (tester) async {
    final fake = FakePeerBeam()
      ..spaceList = const [
        Space(id: 'sp0', name: 'Work', live: ['pb-a'], stale: ['pb-b']),
      ];
    await _open(tester, fake);

    final copy = _copy(tester).toLowerCase();
    for (final forbidden in [
      'invite',
      'join',
      'member',
      'group',
      'room',
      'everyone',
      'participant',
    ]) {
      expect(
        copy,
        isNot(contains(forbidden)),
        reason:
            '"$forbidden" describes a shared roster; a Space has none, and no '
            'peer even knows it exists',
      );
    }
  });

  /// A Space with something live offers Send; the copy still says membership
  /// grants nothing, because that is the claim a device list under a name you
  /// typed could otherwise be read as making.
  testWidgets('a Space with live devices offers Send', (tester) async {
    final fake = FakePeerBeam()
      ..spaceList = const [
        Space(id: 'sp0', name: 'Work', live: ['pb-a', 'pb-b']),
      ];
    await _open(tester, fake);

    expect(find.byKey(const Key('space-sp0-send')), findsOneWidget);
    expect(_copy(tester), contains('being in a Space grants nothing'));
  });

  /// **Absent, not disabled.** The summary line already explains why nothing
  /// can be sent; a dead button invites a tap that can only disappoint.
  testWidgets('a Space with nothing live offers no Send', (tester) async {
    final fake = FakePeerBeam()
      ..spaceList = const [
        Space(id: 'sp0', name: 'Work', stale: ['pb-a']),
      ];
    await _open(tester, fake);

    expect(find.byKey(const Key('space-sp0-send')), findsNothing);
  });

  /// Trusted is not the same as reachable. A member discovery cannot see is
  /// counted rather than silently skipped — believing six people got a file
  /// when four did is the worst outcome this screen could produce.
  testWidgets(
    'sending with nothing reachable says they are not on the network',
    (tester) async {
      final fake = FakePeerBeam()
        ..spaceList = const [
          Space(id: 'sp0', name: 'Work', live: ['pb-a', 'pb-b']),
        ];
      await _open(tester, fake);

      await tester.tap(find.byKey(const Key('space-sp0-send')));
      await tester.pumpAndSettle();

      expect(_copy(tester), contains('not on the network'));
      // No picker was opened: there was nobody to send to.
      expect(
        fake.calls.where((c) => c.startsWith('sendFile')),
        isEmpty,
        reason: 'nothing may be sent when nothing is reachable',
      );
    },
  );

  /// **The first frame is not an empty list.** "No Spaces yet" before the read
  /// answers is the confident falsehood every cold start used to flash.
  testWidgets('the first read shows a spinner, never "No Spaces yet"', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..spaceList = const [Space(id: 'sp0', name: 'Work')];
    final state = AppState.live(fake);
    addTearDown(state.dispose);
    await tester.pumpWidget(
      AppScope(
        state: state,
        child: const MaterialApp(home: SpacesScreen()),
      ),
    );

    expect(find.byType(CircularProgressIndicator), findsOneWidget);
    expect(find.text('No Spaces yet'), findsNothing);
    // The explanation is up before the list is, so the frame is never blank.
    expect(_copy(tester), contains('A Space is a name this device keeps'));

    await tester.pumpAndSettle();
    expect(find.byType(CircularProgressIndicator), findsNothing);
    expect(find.text('Work'), findsOneWidget);
  });

  /// **A read that failed is not a device with no Spaces.** It renders as a
  /// failure, with something to do about it.
  testWidgets('a failed read shows an error and a retry, not an empty state', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..spaceList = const [Space(id: 'sp0', name: 'Work')]
      ..failing.add('spaces');
    await _open(tester, fake);

    expect(find.text('Could not read your Spaces'), findsOneWidget);
    expect(
      find.text('No Spaces yet'),
      findsNothing,
      reason: 'an absence and a failure must never render the same',
    );

    // And the retry is a retry: with the fault cleared, the list arrives.
    fake.failing.remove('spaces');
    await tester.tap(find.text('Try again'));
    await tester.pumpAndSettle();
    expect(find.text('Work'), findsOneWidget);
    expect(find.text('Could not read your Spaces'), findsNothing);
  });

  /// Empty reads as the deliberate default it is — a Space is something you
  /// name, and having named none is not a problem.
  testWidgets('no Spaces at all reads as a starting point', (tester) async {
    await _open(tester, FakePeerBeam());

    expect(find.text('No Spaces yet'), findsOneWidget);
    expect(_copy(tester), contains('on this device only'));
    expect(
      find.widgetWithText(FilledButton, 'New Space'),
      findsOneWidget,
      reason: 'an empty state with nothing to do is a dead end',
    );
  });

  /// A Space holding nothing says exactly that, and asks for the one kind of
  /// device that can go in.
  testWidgets('an empty Space asks for a device it can actually hold', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..spaceList = const [Space(id: 'sp0', name: 'Work')];
    await _open(tester, fake);

    expect(find.text('No devices yet'), findsOneWidget);
    expect(_copy(tester), contains('Add a device you already trust'));
  });

  /// **A stale device is shown beside the live ones, and marked.** Hiding it
  /// would shrink a count with nothing to explain the change; showing it plain
  /// would claim a send reaches it.
  testWidgets('a stale device is listed, marked, and told what would fix it', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..trusted = [
        TrustedDevice(
          id: 'pb-a',
          name: 'laptop',
          fingerprint: 'AA',
          trustedAt: DateTime(2026),
        ),
      ]
      ..spaceList = const [
        Space(id: 'sp0', name: 'Work', live: ['pb-a'], stale: ['pb-gone']),
      ];
    await _open(tester, fake);

    // Both rows are present, and the live one is not relabelled by its
    // neighbour's trouble.
    expect(find.text('laptop'), findsOneWidget);
    expect(find.text('pb-gone'), findsOneWidget);
    expect(find.text('1 device · 1 no longer trusted'), findsOneWidget);

    final copy = _copy(tester);
    expect(copy, contains('No longer trusted, so nothing is sent to it'));
    expect(
      copy,
      contains('Pair with it again, or take it out'),
      reason: 'saying what is wrong without saying what fixes it is half a bug',
    );
  });

  /// **"Nothing can be reached" is not "nothing is in it".** `Space.canSend` is
  /// false either way, and the two want opposite advice.
  testWidgets('a Space whose every device went stale says that, not "empty"', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..spaceList = const [
        Space(id: 'sp0', name: 'Old', stale: ['pb-x', 'pb-y']),
      ];
    await _open(tester, fake);

    expect(_copy(tester), contains('Nothing here can be sent to'));
    expect(find.text('2 no longer trusted'), findsOneWidget);
    expect(
      find.text('No devices yet'),
      findsNothing,
      reason:
          'this Space holds two devices; calling it empty sends the user to '
          'add one they added long ago',
    );
    // Never a zero beside a count either — "0 devices" reads as empty.
    expect(_copy(tester), isNot(contains('0 devices')));
  });

  /// A revoke elsewhere in the app changes the live/stale split with nothing
  /// writing to a Space, so the screen re-reads when trust changes. Otherwise a
  /// revoked device sits here looking reachable until the user leaves and comes
  /// back.
  testWidgets('a trust change re-reads the Spaces', (tester) async {
    final fake = FakePeerBeam()
      ..spaceList = const [
        Space(id: 'sp0', name: 'Work', live: ['pb-a']),
      ];
    final state = await _open(tester, fake);
    expect(find.text('1 device'), findsOneWidget);

    // What a revoke looks like from this screen's side: the engine's next
    // answer moves the device across.
    fake.spaceList = const [
      Space(id: 'sp0', name: 'Work', stale: ['pb-a']),
    ];
    await state.trust.refresh();
    await tester.pumpAndSettle();

    expect(find.text('1 no longer trusted'), findsOneWidget);
    expect(_copy(tester), contains('Nothing here can be sent to'));
  });

  /// The marked-up rows carry two and three lines of copy, so they are checked
  /// at phone width as well: a card that renders its warning as an overflow
  /// error on the narrowest supported layout says nothing at all.
  testWidgets('a stale row still reads at phone width', (tester) async {
    tester.view.physicalSize = const Size(360, 720);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    final fake = FakePeerBeam()
      ..spaceList = const [
        Space(id: 'sp0', name: 'Old', live: ['pb-a'], stale: ['pb-x', 'pb-y']),
      ];
    await _open(tester, fake);

    expect(tester.takeException(), isNull);
    expect(_copy(tester), contains('No longer trusted'));
    expect(find.text('1 device \u00b7 2 no longer trusted'), findsOneWidget);
  });

  /// **The picker has to be told what is already staged.** On Android the
  /// native picker streams each pick into its own cache and prunes that cache
  /// by age on every new pick, so a call that does not name the paths the Send
  /// tray still holds lets it delete that batch out from under the tray. Every
  /// other call site passes `keep:`; this one omitted it, which made "stage a
  /// few files, then send some to a Space" a way to lose the first few.
  ///
  /// Asserted on the argument the channel actually receives, like
  /// `desktop_files_test.dart` does — that a call was made proves nothing here.
  testWidgets('sending to a Space tells the picker what is already staged', (
    tester,
  ) async {
    MethodCall? pick;
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      const MethodChannel('peerbeam/android'),
      (c) async {
        if (c.method == 'pickFiles') pick = c;
        return <Map<String, Object?>>[];
      },
    );
    addTearDown(
      () => TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
          .setMockMethodCallHandler(
            const MethodChannel('peerbeam/android'),
            null,
          ),
    );

    final fake = FakePeerBeam()
      ..spaceList = const [
        Space(id: 'sp0', name: 'Work', live: ['pb-a']),
      ];
    final state = await _open(tester, fake);

    // A Space can only be sent to once discovery has an address for a member.
    fake.emit(
      const DeviceAdded(
        SdkDevice(
          id: 'pb-a',
          name: 'Alice Laptop',
          kind: 'laptop',
          platform: 'linux',
          addresses: ['127.0.0.1'],
          port: 49600,
          online: true,
          latencyMs: 5,
          reachableLan: true,
          reachableRemote: false,
        ),
      ),
    );
    // And the tray has to be holding something for there to be anything to
    // lose.
    state.staging.add([
      StagedFile(path: '/cache/picked/1/a.bin', name: 'a.bin', size: 1),
      StagedFile(path: '/cache/picked/1/b.bin', name: 'b.bin', size: 2),
    ]);
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('space-sp0-send')));
    await tester.pumpAndSettle();

    expect(pick, isNotNull, reason: 'the picker was never opened');
    // Matched rather than cast, so omitting `keep:` altogether — which sends no
    // arguments at all — reads as a failed expectation and not a type error.
    expect(
      pick!.arguments,
      isA<Map<Object?, Object?>>().having((m) => m['keep'], 'keep', [
        '/cache/picked/1/a.bin',
        '/cache/picked/1/b.bin',
      ]),
      reason: 'the picker was not told what the Send tray still holds',
    );
  });
}
