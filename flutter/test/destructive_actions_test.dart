// Nothing destructive in this app used to ask, and nothing could be taken back:
// until this suite existed there was exactly one `SnackBarAction` in `lib/`
// ("Copy path", on the logs screen) and confirmations only in front of trust
// and history. Four actions deleted something on a single tap.
//
// The fix is deliberately not uniform, and these tests pin which shape each one
// got, because applying the wrong one is its own failure:
//
//  * **Prompt** where the thing cannot be reconstructed — a saved device is a
//    hand-typed `host:port` nothing on the machine remembers, and a staged text
//    message exists only in the tray.
//  * **Undo** where it can — a save rule and a shared folder are both fully
//    described by the row that was showing them, and a dialog on every delete
//    is a dialog people stop reading.
//
// So each undo case asserts *no* prompt appeared, as firmly as each prompt case
// asserts one did.

import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'package:peerbeam/features/home/home_screen.dart';
import 'package:peerbeam/features/send/staged_sheet.dart';
import 'package:peerbeam/features/settings/settings_screen.dart';
import 'package:peerbeam/sdk/exceptions.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/staging.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

/// Pump [child] under a live [AppState] and return the state.
Future<AppState> _live(
  WidgetTester tester,
  Widget child,
  FakePeerBeam fake,
) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: MaterialApp(home: child),
    ),
  );
  return state;
}

/// `pumpAndSettle` is unusable on these screens — the app state keeps timers
/// running, so "settled" never arrives and a test would sit until the suite
/// timeout instead of failing usefully. Two fixed pumps clear a route
/// transition and a snackbar's entrance.
Future<void> _tick(WidgetTester tester) async {
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 400));
}

// ---------------------------------------------------------------------------
// Saved device — prompt. The address is unrecoverable.
// ---------------------------------------------------------------------------

/// Open a saved device's action menu, with one device saved.
Future<AppState> _openSavedMenu(WidgetTester tester) async {
  final state = await _live(tester, const HomeScreen(), FakePeerBeam());
  await state.saved.add(name: 'Server', host: '10.0.0.5', port: 49600);
  await _tick(tester);
  await tester.tap(find.byTooltip('Device actions'));
  await _tick(tester);
  await tester.tap(find.text('Remove'));
  await _tick(tester);
  return state;
}

// ---------------------------------------------------------------------------
// Staged tray — prompt. A typed message has no source to re-pick from.
// ---------------------------------------------------------------------------

/// Open the staged sheet over a live state holding [staging] items.
Future<AppState> _openTray(WidgetTester tester, List<StagedFile> items) async {
  late BuildContext ctx;
  final state = await _live(
    tester,
    Scaffold(
      body: Builder(
        builder: (c) {
          ctx = c;
          return const SizedBox();
        },
      ),
    ),
    FakePeerBeam(),
  );
  state.staging.add(items);
  await _tick(tester);
  unawaited(showStagedFilesSheet(ctx, state.staging));
  await _tick(tester);
  return state;
}

// ---------------------------------------------------------------------------
// Settings — undo. Both rows describe themselves completely.
// ---------------------------------------------------------------------------

/// Open Settings with [fake]'s document loaded and [scrollTo] in view.
Future<AppState> _openSettings(
  WidgetTester tester,
  FakePeerBeam fake,
  String scrollTo,
) async {
  final state = await _live(tester, const SettingsScreen(), fake);
  await tester.pump();
  await state.settings.load(fake);
  await tester.pump();
  await tester.scrollUntilVisible(
    find.text(scrollTo),
    300,
    scrollable: find.byType(Scrollable).first,
  );
  await _tick(tester);
  return state;
}

Map<String, dynamic> _rule(String directory, {String? extension}) => {
  'extension': ?extension,
  'directory': directory,
};

void main() {
  // Every one of these persists through SharedPreferences, whose platform
  // channel never answers in a widget test unless it is primed: without this
  // the await simply never returns and the test hangs rather than failing.
  setUp(() => SharedPreferences.setMockInitialValues({}));

  group('removing a saved device asks first', () {
    testWidgets('the prompt names the address that is about to be lost', (
      tester,
    ) async {
      await _openSavedMenu(tester);

      expect(find.text('Remove Server?'), findsOneWidget);
      expect(
        // Scoped to the dialog: the card behind it shows the same address, so
        // an unscoped finder would pass without the prompt saying anything.
        find.descendant(
          of: find.byType(AlertDialog),
          matching: find.textContaining('10.0.0.5:49600'),
        ),
        findsOneWidget,
        reason:
            'the address is the thing nothing else remembers — it has to be '
            'readable while there is still time to read it',
      );
    });

    testWidgets('declining keeps the device', (tester) async {
      final state = await _openSavedMenu(tester);

      await tester.tap(find.widgetWithText(TextButton, 'Cancel'));
      await _tick(tester);

      expect(state.saved.devices, hasLength(1));
      expect(state.saved.devices.single.host, '10.0.0.5');
      expect(find.text('Server'), findsOneWidget);
    });

    testWidgets('confirming removes it', (tester) async {
      final state = await _openSavedMenu(tester);

      await tester.tap(find.widgetWithText(FilledButton, 'Remove'));
      await _tick(tester);

      expect(state.saved.devices, isEmpty);
      expect(find.text('Server'), findsNothing);
    });
  });

  group('clearing the staged tray asks first', () {
    testWidgets('declining keeps every item', (tester) async {
      final state = await _openTray(tester, [
        StagedFile(path: '/x/a.bin', name: 'a.bin', size: 5),
        StagedFile(path: '/x/b.bin', name: 'b.bin', size: 7),
      ]);

      await tester.tap(find.widgetWithText(TextButton, 'Clear'));
      await _tick(tester);
      expect(find.text('Clear 2 items?'), findsOneWidget);

      await tester.tap(find.widgetWithText(TextButton, 'Cancel'));
      await _tick(tester);

      expect(state.staging.count, 2);
      await tester.pumpWidget(const SizedBox());
    });

    testWidgets('confirming empties it', (tester) async {
      final state = await _openTray(tester, [
        StagedFile(path: '/x/a.bin', name: 'a.bin', size: 5),
      ]);
      state.staging.addText('the paragraph nothing else holds');
      await _tick(tester);

      await tester.tap(find.widgetWithText(TextButton, 'Clear'));
      await _tick(tester);
      // Singular/plural is stated because the count is the whole warning.
      expect(find.text('Clear 2 items?'), findsOneWidget);

      await tester.tap(find.widgetWithText(FilledButton, 'Clear'));
      await _tick(tester);

      expect(state.staging.isEmpty, isTrue);
      await tester.pumpWidget(const SizedBox());
    });
  });

  group('removing a save rule offers an undo', () {
    testWidgets('the rule goes back at the index it left, not at the end', (
      tester,
    ) async {
      final fake = FakePeerBeam()
        ..settings.addAll({
          'rules_supported': true,
          'save_rules': [
            _rule('/srv/papers', extension: 'pdf'),
            _rule('/srv/inbox'),
          ],
        });
      final state = await _openSettings(tester, fake, 'AUTO-SAVE RULES');

      final remove = find.descendant(
        of: find.byKey(const ValueKey('rule-0-/srv/papers')),
        matching: find.byTooltip('Remove rule'),
      );
      await tester.ensureVisible(remove);
      await _tick(tester);
      await tester.tap(remove);
      await _tick(tester);

      // No prompt: this is the undo case, and a dialog on a row whose delete
      // button sits in a drag-to-reorder list is one people learn to dismiss.
      expect(find.byType(AlertDialog), findsNothing);
      expect(
        state.settings.saveRules.map((r) => r.directory),
        ['/srv/inbox'],
        reason: 'the removal itself must not wait for the undo to expire',
      );

      await tester.tap(find.text('Undo'));
      await _tick(tester);

      expect(
        state.settings.saveRules.map((r) => r.directory),
        ['/srv/papers', '/srv/inbox'],
        reason:
            'the first match wins, so a rule restored at the end would claim '
            'different files than the one that was removed',
      );
      expect(
        fake.rulesWritten.map((r) => r.directory),
        ['/srv/papers', '/srv/inbox'],
        reason: 'and the restored order reached the engine, not just the UI',
      );
    });

    testWidgets('leaving the undo alone leaves the rule removed', (
      tester,
    ) async {
      final fake = FakePeerBeam()
        ..settings.addAll({
          'rules_supported': true,
          'save_rules': [_rule('/srv/papers', extension: 'pdf')],
        });
      final state = await _openSettings(tester, fake, 'AUTO-SAVE RULES');

      final remove = find.byTooltip('Remove rule');
      await tester.ensureVisible(remove);
      await _tick(tester);
      await tester.tap(remove);
      await _tick(tester);

      // Past the snackbar's own lifetime: an undo that fired on its way out
      // would silently resurrect what the user meant to delete.
      await tester.pump(const Duration(seconds: 10));

      expect(state.settings.saveRules, isEmpty);
      expect(find.text('No rules'), findsOneWidget);
    });

    testWidgets('a refused removal reports the refusal and offers no undo', (
      tester,
    ) async {
      // An Undo beside a write the engine rejected would be offering to undo
      // nothing, on top of a rule that is still in force.
      final fake = FakePeerBeam()
        ..settings.addAll({
          'rules_supported': true,
          'save_rules': [_rule('/srv/papers', extension: 'pdf')],
        })
        ..rulesError = const InvalidArgumentException('rule 1: nope');
      final state = await _openSettings(tester, fake, 'AUTO-SAVE RULES');

      final remove = find.byTooltip('Remove rule');
      await tester.ensureVisible(remove);
      await _tick(tester);
      await tester.tap(remove);
      await _tick(tester);

      expect(find.text('Undo'), findsNothing);
      expect(state.settings.saveRules.map((r) => r.directory), ['/srv/papers']);
      expect(find.textContaining('nope'), findsOneWidget);
    });
  });

  group('stopping a share offers an undo', () {
    testWidgets('un-sharing is immediate and unprompted, and can be taken '
        'back where it was', (tester) async {
      // Pinned to desktop: flutter_test reports Android by default, and the
      // shared-folders card draws no list there — Android reaches a folder
      // only through a grant it cannot turn into a path, so it is given that
      // reason in place of an editor. The undo under test is a desktop one.
      // Reset in a `finally`, not an `addTearDown`: `testWidgets` re-checks
      // foundation's debug variables the instant this callback returns, before
      // the tear-down queue unwinds.
      debugDefaultTargetPlatformOverride = TargetPlatform.linux;
      try {
        final fake = FakePeerBeam()
          ..shares = const [
            SharedFolder(name: 'Photos', path: '/home/me/Photos', exists: true),
            SharedFolder(name: 'Media', path: '/srv/media', exists: true),
          ];
        await _openSettings(tester, fake, 'SHARED FOLDERS');

        await tester.tap(
          find.descendant(
            of: find.byKey(const Key('shared-folder-/srv/media')),
            matching: find.byTooltip('Stop sharing'),
          ),
        );
        await _tick(tester);

        // Un-sharing is the safe direction: it must never be slowed by a
        // prompt.
        expect(find.byType(AlertDialog), findsNothing);
        expect(fake.calls, contains('setSharedFolders:/home/me/Photos'));
        expect(find.text('/srv/media'), findsNothing);

        await tester.tap(find.text('Undo'));
        await _tick(tester);

        expect(
          fake.calls,
          // Not `calls.last`: every write is followed by a re-read, so the
          // last call is always the read.
          contains('setSharedFolders:/home/me/Photos,/srv/media'),
          reason: 'restored at the index it left, not appended',
        );
        expect(find.text('/srv/media'), findsOneWidget);
      } finally {
        debugDefaultTargetPlatformOverride = null;
      }
    });
  });
}
