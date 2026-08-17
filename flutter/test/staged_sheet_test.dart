// staged_sheet.dart's "Files" button (`_addFiles`) is the exact repro path
// for the picked-cache bug: Android's `preparePickedDir` streams every pick
// into `cacheDir/picked` and used to wipe that whole directory on the very
// next pick, even though the Send flow only reads a staged file's path back
// at Send time — so picking again from inside the sheet while an earlier
// batch is still staged there destroyed that earlier batch's bytes. The fix
// is `keep`: the paths already in the sheet, threaded down to the native
// picker so it never prunes a batch still in use. These tests assert the
// actual `keep` argument the channel receives, not merely that some call
// was made.

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/send/staged_sheet.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/staging.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

Future<BuildContext> _openSheetHarness(
  WidgetTester tester,
  AppState state,
) async {
  late BuildContext ctx;
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: MaterialApp(
        home: Scaffold(
          body: Builder(
            builder: (c) {
              ctx = c;
              return const SizedBox();
            },
          ),
        ),
      ),
    ),
  );
  unawaited(showStagedFilesSheet(ctx, state.staging));
  await tester.pumpAndSettle();
  return ctx;
}

void main() {
  testWidgets(
    'picking from inside the sheet while a file is already staged there '
    'sends that file\'s path as keep — the exact repro: A staged, tap '
    'Files, pick B must not let the native side prune A',
    (tester) async {
      final calls = <MethodCall>[];
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        const MethodChannel('peerbeam/android'),
        (call) async {
          if (call.method == 'pickFiles') calls.add(call);
          return <Map<String, Object?>>[
            {'path': '/cache/picked/2/b.mp4', 'name': 'b.mp4', 'size': 200},
          ];
        },
      );

      final state = AppState.live(FakePeerBeam());
      addTearDown(state.dispose);
      state.staging.add([
        StagedFile(path: '/cache/picked/1/a.mp4', name: 'a.mp4', size: 100),
      ]);

      await _openSheetHarness(tester, state);

      await tester.tap(find.text('Files'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));

      expect(calls, hasLength(1));
      expect((calls.single.arguments as Map)['keep'], [
        '/cache/picked/1/a.mp4',
      ]);

      // The pick landed for real — both files are staged now — so this
      // exercised the actual add-to-staging path, not a stub that never
      // wired the result back in.
      expect(state.staging.items.map((f) => f.path), [
        '/cache/picked/1/a.mp4',
        '/cache/picked/2/b.mp4',
      ]);

      // Tear the tree down so nothing from the modal sheet's route survives
      // into the next test.
      await tester.pumpWidget(const SizedBox());
    },
  );

  testWidgets(
    'with nothing staged, picking from inside the empty sheet sends no keep '
    'argument and still works',
    (tester) async {
      final calls = <MethodCall>[];
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        const MethodChannel('peerbeam/android'),
        (call) async {
          if (call.method == 'pickFiles') calls.add(call);
          return <Map<String, Object?>>[];
        },
      );

      final state = AppState.live(FakePeerBeam());
      addTearDown(state.dispose);

      await _openSheetHarness(tester, state);

      await tester.tap(find.text('Files'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));

      expect(calls, hasLength(1));
      expect(calls.single.arguments, isNull);

      await tester.pumpWidget(const SizedBox());
    },
  );
}
