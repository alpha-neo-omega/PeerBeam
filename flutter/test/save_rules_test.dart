// The Auto-save rules section: where a received file lands, never whether it
// is accepted.
//
// Two properties this screen exists to make visible, and which these tests pin:
//
//  1. **The order is the tie-break.** The first rule that matches wins, so the
//     list is reorderable and what the engine is sent must be the order shown.
//  2. **A rule is not an acceptance filter.** The section says so in as many
//     words, because a list of match rules sitting beneath "Auto-accept trusted
//     devices" would otherwise read like one.
//
// Plus the platform half: where the engine reports it cannot honour rules
// (Android), there is no editor and the section says why instead of silently
// doing nothing.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/settings/settings_screen.dart';
import 'package:peerbeam/sdk/exceptions.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

Map<String, dynamic> _rule({
  String? device,
  String? extension,
  int? minBytes,
  int? maxBytes,
  required String directory,
}) => {
  'device': ?device,
  'extension': ?extension,
  'min_bytes': ?minBytes,
  'max_bytes': ?maxBytes,
  'directory': directory,
};

/// Open Settings with the fake's settings document loaded, and scroll the
/// Auto-save rules section into view.
///
/// The load is explicit for the same reason the trusted-devices test's refresh
/// is: the store is constructed before the engine is known, so nothing is
/// fetched in its constructor.
Future<AppState> _open(
  WidgetTester tester,
  FakePeerBeam fake, {
  String scrollTo = 'AUTO-SAVE RULES',
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
  await state.settings.load(fake);
  await tester.pump();
  await tester.scrollUntilVisible(
    find.text(scrollTo),
    300,
    scrollable: find.byType(Scrollable).first,
  );
  await tester.pumpAndSettle();
  return state;
}

void main() {
  /// **The copy that keeps the feature honest.** A rule chooses where; it does
  /// not choose whether. Softening this to "rules for received files" would let
  /// the section read as an acceptance filter sitting one card below the
  /// auto-accept switch.
  testWidgets(
    'the section says a rule never decides whether a file is accepted',
    (tester) async {
      final fake = FakePeerBeam()
        ..settings.addAll({'rules_supported': true, 'save_rules': const []});
      await _open(tester, fake);

      final copy = tester
          .widgetList<Text>(find.byType(Text))
          .map((t) => t.data ?? '')
          .join(' ');
      expect(
        copy,
        contains('never decide'),
        reason: 'the section must disclaim acceptance in as many words',
      );
      expect(
        copy,
        contains('first rule that matches'),
        reason: 'and state the tie-break, since ordering is the whole model',
      );
    },
  );

  /// With no rules the section says where files go instead — the answer to
  /// "what happens to my files?" without needing to know rules exist.
  testWidgets('with no rules the section names the fallback folder', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..settings.addAll({
        'rules_supported': true,
        'save_rules': const [],
        'transfer_directory': '/home/me/Downloads',
      });
    await _open(tester, fake);

    expect(find.text('No rules'), findsOneWidget);
    final copy = tester
        .widgetList<Text>(find.byType(Text))
        .map((t) => t.data ?? '')
        .join(' ');
    expect(copy, contains('/home/me/Downloads'));
  });

  /// Rules render in the stored order, with their criteria legible.
  testWidgets('rules render in order with their criteria', (tester) async {
    final fake = FakePeerBeam()
      ..settings.addAll({
        'rules_supported': true,
        'save_rules': [
          _rule(extension: 'pdf', directory: '/srv/papers'),
          _rule(device: 'pb-laptop00001', directory: '/srv/from-laptop'),
          _rule(directory: '/srv/inbox'),
        ],
      });
    await _open(tester, fake);

    expect(find.text('*.pdf'), findsOneWidget);
    expect(find.text('From pb-laptop00001'), findsOneWidget);
    // A criteria-less rule is named as what it is: the single most
    // consequential thing about it is that it matches everything.
    expect(find.text('Everything'), findsOneWidget);
    expect(find.text('/srv/papers'), findsOneWidget);
  });

  /// **Order is the tie-break, so reordering must reach the engine.** The list
  /// the engine is sent has to be the list on screen, in that order.
  testWidgets('dragging a rule sends the reordered list to the engine', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..settings.addAll({
        'rules_supported': true,
        'save_rules': [
          _rule(extension: 'pdf', directory: '/srv/papers'),
          _rule(directory: '/srv/inbox'),
        ],
      });
    final state = await _open(tester, fake);

    // Drive the reorder through the store, the same call the drag handle makes
    // — a synthetic long-press-drag on a ReorderableListView is a gesture test,
    // not a test of what gets persisted.
    await state.settings.setSaveRules([
      state.settings.saveRules[1],
      state.settings.saveRules[0],
    ]);
    await tester.pumpAndSettle();

    expect(
      fake.rulesWritten.map((r) => r.directory).toList(),
      ['/srv/inbox', '/srv/papers'],
      reason: 'the engine must receive the new order, not the old one',
    );
    expect(state.settings.saveRules.first.directory, '/srv/inbox');
  });

  /// A catch-all makes every rule below it unreachable. The list says so beside
  /// the rule it affects rather than leaving the user to discover it.
  testWidgets('a rule shadowed by a catch-all above it is marked', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..settings.addAll({
        'rules_supported': true,
        'save_rules': [
          _rule(directory: '/srv/inbox'),
          _rule(extension: 'pdf', directory: '/srv/papers'),
        ],
      });
    await _open(tester, fake);

    expect(find.textContaining('Never reached'), findsOneWidget);
  });

  /// **A refused write must not be shown as applied.** The engine validates
  /// destinations, and if it refuses, the list on screen must still be the list
  /// actually in force — otherwise the user believes files are being sorted
  /// that are not.
  testWidgets('a rejected rule leaves the shown list unchanged', (
    tester,
  ) async {
    final fake = FakePeerBeam()
      ..settings.addAll({
        'rules_supported': true,
        'save_rules': [_rule(directory: '/srv/inbox')],
      })
      ..rulesError = const InvalidArgumentException(
        'rule 1: destination must be an absolute path: nope',
      );
    final state = await _open(tester, fake);

    await expectLater(
      state.settings.setSaveRules([
        ...state.settings.saveRules,
        const SaveRule(directory: 'nope'),
      ]),
      throwsA(isA<InvalidArgumentException>()),
    );
    expect(
      state.settings.saveRules.map((r) => r.directory).toList(),
      ['/srv/inbox'],
      reason: 'a refused write must not be adopted locally',
    );
  });

  /// **A platform that cannot honour rules is told, not left guessing.** No
  /// editor, and the reason in plain words — a section that silently did
  /// nothing would be worse than no section at all.
  ///
  /// Driven by the engine's `rules_supported`, not by a platform check in the
  /// UI: the engine is the one that knows whether it can write to an arbitrary
  /// absolute path, and a second opinion here could disagree with it.
  testWidgets(
    'an unsupported platform explains itself instead of showing an editor',
    (tester) async {
      final fake = FakePeerBeam()
        ..settings.addAll({'rules_supported': false, 'save_rules': const []});
      await _open(tester, fake);

      expect(find.text('Not available on this device'), findsOneWidget);
      expect(find.text('Add rule'), findsNothing);
      final copy = tester
          .widgetList<Text>(find.byType(Text))
          .map((t) => t.data ?? '')
          .join(' ');
      expect(
        copy,
        contains('cannot write to any other location'),
        reason: 'the limitation must be explained, not merely applied',
      );
    },
  );

  /// An engine that predates the field reports no `rules_supported`, and the
  /// UI must read that as "no", never as "yes". Offering an editor that cannot
  /// work is the failure mode this default exists to prevent.
  testWidgets('an engine that predates the flag is treated as unsupported', (
    tester,
  ) async {
    final fake = FakePeerBeam()..settings.addAll({'device_name': 'x'});
    await _open(tester, fake);

    expect(find.text('Not available on this device'), findsOneWidget);
  });

  /// The model's shape, where the engine contract lives: an unset criterion is
  /// **omitted**, never `""` or `0` — a blank criterion matches nothing, and
  /// `0` is a legitimate minimum size.
  test('an unset criterion is omitted from the engine payload', () {
    const rule = SaveRule(directory: '/srv/inbox');
    expect(rule.toJson(), {'directory': '/srv/inbox'});
    expect(rule.isCatchAll, isTrue);

    const full = SaveRule(
      deviceId: 'pb-laptop00001',
      extension: 'pdf',
      minBytes: 0,
      maxBytes: 10,
      directory: '/srv/papers',
    );
    expect(full.toJson(), {
      'device': 'pb-laptop00001',
      'extension': 'pdf',
      'min_bytes': 0,
      'max_bytes': 10,
      'directory': '/srv/papers',
    });
    expect(full.isCatchAll, isFalse);
  });
}
