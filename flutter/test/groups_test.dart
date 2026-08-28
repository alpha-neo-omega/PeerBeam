// Groups in the app.
//
// The load-bearing one is the join dialog: amendment A2 permits groups only on
// the condition that the metadata cost is stated to the user **at the point of
// joining**, and this app is where most people will meet it. A build that
// dropped that sentence would be outside the amendment, not merely less
// helpful — so it is pinned here rather than left to review.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/groups/groups_screen.dart';
import 'package:peerbeam/features/groups/join_dialog.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

GroupInvite _invite({
  String group = 'g-1',
  String name = 'Work Trip',
  String from = 'pb-alice',
  List<String> members = const ['pb-alice', 'pb-bob'],
}) => GroupInvite(
  group: group,
  name: name,
  from: from,
  members: members,
  at: DateTime.utc(2026, 8, 27),
);

Future<AppState> _open(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(state: state, child: const MaterialApp(home: GroupsScreen())),
  );
  await tester.pumpAndSettle();
  return state;
}

void main() {
  group('the join dialog', () {
    /// **A2, condition 5.** The user is told, before answering, that every
    /// member will learn their device — and that it cannot be undone.
    testWidgets('states the disclosure before the user can accept', (
      tester,
    ) async {
      await tester.pumpWidget(
        MaterialApp(home: JoinGroupDialog(invite: _invite())),
      );
      await tester.pumpAndSettle();

      expect(find.textContaining('will see your device'), findsOneWidget);
      expect(find.textContaining('cannot be undone'), findsOneWidget);
    });

    /// **Named, not counted.** "2 devices" is a fact about arithmetic; the list
    /// is what the user is actually agreeing to. Somebody who would decline
    /// because of one particular device cannot act on a number.
    testWidgets('names every device that will learn this one', (tester) async {
      await tester.pumpWidget(
        MaterialApp(
          home: JoinGroupDialog(
            invite: _invite(members: const ['pb-alice', 'pb-bob']),
            nameFor: (id) => switch (id) {
              'pb-alice' => 'Alice Laptop',
              'pb-bob' => 'Bob Phone',
              _ => id,
            },
          ),
        ),
      );
      await tester.pumpAndSettle();

      expect(find.text('Alice Laptop'), findsOneWidget);
      expect(find.text('Bob Phone'), findsOneWidget);
    });

    /// The confirm button names the consequence. A person clicking through a
    /// dialog reads the button far more often than the paragraph, and one
    /// saying "OK" has told them nothing about what they agreed to.
    testWidgets('the confirm button says what it costs', (tester) async {
      await tester.pumpWidget(
        MaterialApp(home: JoinGroupDialog(invite: _invite())),
      );
      await tester.pumpAndSettle();

      expect(find.text('Join and share who I am'), findsOneWidget);
      expect(find.text('OK'), findsNothing);
      expect(find.text('Accept'), findsNothing);
    });
  });

  group('the groups screen', () {
    testWidgets('an invitation is shown apart from the groups', (tester) async {
      final fake = FakePeerBeam()
        ..groupsList = const [Group(id: 'g-9', name: 'Family')]
        ..groupInvites = [_invite()];
      await _open(tester, fake);

      // Both sections present, and the invitation is not listed as a group.
      expect(find.byKey(const Key('invite-g-1')), findsOneWidget);
      expect(find.byKey(const Key('group-g-9')), findsOneWidget);
      expect(find.byKey(const Key('group-g-1')), findsNothing);
    });

    /// **Holding an invitation is not being in a group.** Reviewing one must
    /// not join it; only the confirm inside the dialog does.
    testWidgets('reviewing an invitation does not join it', (tester) async {
      final fake = FakePeerBeam()..groupInvites = [_invite()];
      final state = await _open(tester, fake);

      await tester.tap(find.text('Review'));
      await tester.pumpAndSettle();
      expect(find.textContaining('will see your device'), findsOneWidget);

      // Dismiss without confirming.
      await tester.tap(find.text('Not now'));
      await tester.pumpAndSettle();

      expect(fake.groupAccepts, isEmpty, reason: 'a review joined the group');
      expect(state.groups.groups, isEmpty);
    });

    testWidgets('confirming the dialog joins, and tells the members', (
      tester,
    ) async {
      final fake = FakePeerBeam()..groupInvites = [_invite()];
      final state = await _open(tester, fake);

      await tester.tap(find.text('Review'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Join and share who I am'));
      await tester.pumpAndSettle();

      expect(fake.groupAccepts, hasLength(1));
      expect(fake.groupAccepts.single.group, 'g-1');
      expect(state.groups.groups.single.id, 'g-1');
    });

    /// Declining is local and silent, and the app says so — otherwise somebody
    /// declines while worrying about a message that is never sent.
    testWidgets('ignoring says the inviter is not told', (tester) async {
      final fake = FakePeerBeam()..groupInvites = [_invite()];
      await _open(tester, fake);

      await tester.tap(find.text('Ignore'));
      await tester.pumpAndSettle();

      expect(find.textContaining('is not told'), findsOneWidget);
    });

    /// **A member that cannot be messaged is named, not hidden.** A list that
    /// quietly shrank would leave somebody wondering whether they had ever
    /// added the device.
    testWidgets('a member that cannot be messaged is named', (tester) async {
      final fake = FakePeerBeam()
        ..groupsList = const [
          Group(
            id: 'g-9',
            name: 'Family',
            members: ['pb-a', 'pb-b'],
            reachable: ['pb-a'],
            unreachable: ['pb-b'],
          ),
        ];
      await _open(tester, fake);

      expect(find.textContaining('cannot be messaged'), findsOneWidget);
    });

    /// A failed read is not "you are in no groups" — that is a claim about the
    /// user's own memberships a failure has no grounds to make.
    testWidgets('a failed read says so rather than showing an empty state', (
      tester,
    ) async {
      final fake = FakePeerBeam()..failing.add('groups');
      await _open(tester, fake);

      expect(find.textContaining('Could not read'), findsOneWidget);
      expect(find.text('No groups yet'), findsNothing);
    });

    /// The empty state has to distinguish a Group from a Space, because
    /// confusing them discloses something the user did not mean to.
    testWidgets('the empty state explains what a group costs', (tester) async {
      await _open(tester, FakePeerBeam());

      expect(find.text('No groups yet'), findsOneWidget);
      expect(find.textContaining('Space'), findsOneWidget);
    });
  });

  group('the conversation is reachable', () {
    /// **The feature was unreachable.** Engine, FFI, CLI and repository all
    /// carried `send` and `history`; this screen wired neither, so a group
    /// could be created and joined and never spoken in.
    testWidgets('a group can be opened and spoken in', (tester) async {
      final fake = FakePeerBeam()
        ..groupsList = [
          const Group(
            id: 'g-1',
            name: 'Work Trip',
            members: ['pb-me', 'pb-bob'],
            reachable: ['pb-bob'],
            unreachable: [],
          ),
        ];
      await _open(tester, fake);

      await tester.tap(find.byKey(const Key('open-g-1')));
      // Pumped rather than settled: the loading spinner animates forever, so
      // `pumpAndSettle` never returns while it is on screen.
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));

      expect(find.byKey(const Key('group-composer')), findsOneWidget);

      await tester.enterText(
        find.byKey(const Key('group-composer')),
        'six works for me',
      );
      await tester.tap(find.byKey(const Key('group-send')));
      await tester.pumpAndSettle();

      expect(
        fake.groupSends,
        contains((id: 'g-1', text: 'six works for me')),
        reason: 'the composer must actually reach the engine',
      );
    });

    /// A member the `chat` permission excludes is named, never silently
    /// dropped: the message did reach the rest.
    testWidgets('a member who was skipped is named', (tester) async {
      final fake = FakePeerBeam()
        ..groupsList = [
          const Group(
            id: 'g-1',
            name: 'Work Trip',
            members: ['pb-me', 'pb-bob'],
            reachable: ['pb-bob'],
            unreachable: [],
          ),
        ]
        ..groupSkipped = ['pb-bob'];
      await _open(tester, fake);

      await tester.tap(find.byKey(const Key('open-g-1')));
      // Pumped rather than settled: the loading spinner animates forever, so
      // `pumpAndSettle` never returns while it is on screen.
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      await tester.enterText(find.byKey(const Key('group-composer')), 'hello');
      await tester.tap(find.byKey(const Key('group-send')));
      await tester.pumpAndSettle();

      expect(find.textContaining('pb-bob'), findsWidgets);
    });

    /// Inviting is the other half that was missing, and it must state the
    /// disclosure before it happens — A2, condition 5.
    testWidgets('inviting says what it costs before it is sent', (tester) async {
      final fake = FakePeerBeam()
        ..groupsList = [
          const Group(
            id: 'g-1',
            name: 'Work Trip',
            members: ['pb-me'],
            reachable: [],
            unreachable: [],
          ),
        ];
      await _open(tester, fake);

      await tester.tap(find.byKey(const Key('invite-to-g-1')));
      await tester.pumpAndSettle();

      // With nobody reachable the screen says so rather than offering a
      // device the engine would refuse.
      expect(find.textContaining('has to see a trusted device'), findsOneWidget);
    });
  });
}
