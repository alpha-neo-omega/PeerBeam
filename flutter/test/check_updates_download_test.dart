import 'dart:async';

// Not a new dependency: file_selector_platform_interface already resolves
// through file_selector, and faking it is how the other picker tests answer
// the folder dialog without a real one.
// ignore: depend_on_referenced_packages
import 'package:file_selector_platform_interface/file_selector_platform_interface.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/features/settings/check_updates_tile.dart';
import 'package:peerbeam/features/settings/settings_screen.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

/// Downloading a release from the "Check for updates" tile.
///
/// Amendment A3 permits the download on terms the tile has to keep, and most
/// of these tests are one of those terms: nothing is fetched as a side effect
/// of a check (condition 1), only into a folder the person chose ("written to
/// a location that person chose"), and the file is never opened -- the folder
/// is (condition 4).
void main() {
  const available = UpdateCheck(
    reachable: true,
    current: '0.9.0',
    latest: '0.9.1',
    updateAvailable: true,
    url: 'https://peerbeam.pages.dev/download',
  );
  const folder = '/home/me/Downloads';
  const saved = UpdateDownload(
    ok: true,
    downloaded: true,
    current: '0.9.0',
    latest: '0.9.1',
    path: '/home/me/Downloads/peerbeam-0.9.1-amd64.deb',
    name: 'peerbeam-0.9.1-amd64.deb',
    bytes: 12901854,
  );

  /// Answers the folder picker, and remembers being asked.
  _Picker usePicker(String? answer) {
    final real = FileSelectorPlatform.instance;
    final picker = _Picker(answer);
    FileSelectorPlatform.instance = picker;
    addTearDown(() => FileSelectorPlatform.instance = real);
    return picker;
  }

  /// The tile as it really appears: inside Settings, under About.
  Future<void> openSettings(WidgetTester tester, FakePeerBeam fake) async {
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
      find.text('Check for updates'),
      300,
      scrollable: find.byType(Scrollable).first,
    );
    await tester.pumpAndSettle();
  }

  Future<void> check(WidgetTester tester) async {
    await tester.tap(find.text('Check'));
    await tester.pumpAndSettle();
  }

  bool fetched(FakePeerBeam fake) =>
      fake.calls.any((c) => c.startsWith('downloadUpdate'));

  testWidgets(
    'a newer release offers Download, and finding one fetches nothing',
    (tester) async {
      // A picker that *would* answer: if finding a release started a download
      // by itself, nothing here would stop it reaching the engine.
      final picker = usePicker(folder);
      final fake = FakePeerBeam()..updateCheck = available;
      await openSettings(tester, fake);
      await check(tester);

      expect(find.text('Download'), findsOneWidget);
      expect(
        picker.asked,
        0,
        reason: 'A3 condition 1: a check must not even ask where to save',
      );
      expect(
        fetched(fake),
        isFalse,
        reason: 'A3 condition 1: no download on the heels of a check',
      );
    },
    variant: TargetPlatformVariant.desktop(),
  );

  testWidgets(
    'nothing is offered to download when this build is current',
    (tester) async {
      final fake = FakePeerBeam(); // reachable, and current
      await openSettings(tester, fake);
      await check(tester);

      expect(find.text('Download'), findsNothing);
    },
    variant: TargetPlatformVariant.desktop(),
  );

  testWidgets(
    'Download asks where to save it, and fetches into that folder',
    (tester) async {
      final picker = usePicker(folder);
      final fake = FakePeerBeam()..updateCheck = available;
      await openSettings(tester, fake);
      await check(tester);
      await tester.tap(find.text('Download'));
      await tester.pumpAndSettle();

      expect(picker.asked, 1);
      expect(fake.calls, contains('downloadUpdate:$folder'));
    },
    variant: TargetPlatformVariant.desktop(),
  );

  testWidgets(
    'cancelling the folder picker downloads nothing',
    (tester) async {
      usePicker(null);
      final fake = FakePeerBeam()..updateCheck = available;
      await openSettings(tester, fake);
      await check(tester);
      await tester.tap(find.text('Download'));
      await tester.pumpAndSettle();

      expect(
        fetched(fake),
        isFalse,
        reason: 'A3: the file goes only to a location the person chose',
      );
      expect(find.text('Download'), findsOneWidget, reason: 'still on offer');
    },
    variant: TargetPlatformVariant.desktop(),
  );

  testWidgets(
    'progress is shown while it downloads',
    (tester) async {
      usePicker(folder);
      final fake = FakePeerBeam()
        ..updateCheck = available
        ..updateDownload = saved
        ..downloadGate = Completer<void>();
      await openSettings(tester, fake);
      await check(tester);
      await tester.tap(find.text('Download'));
      await tester.pump();
      await tester.pump();

      fake.emit(const UpdateDownloadProgress(done: 5242880, total: 10485760));
      await tester.pump();
      expect(find.textContaining('50%'), findsOneWidget);

      fake.downloadGate!.complete();
      await tester.pumpAndSettle();
      expect(find.textContaining('peerbeam-0.9.1-amd64.deb'), findsOneWidget);
    },
    variant: TargetPlatformVariant.desktop(),
  );

  testWidgets(
    'Show folder opens the folder that was chosen, never the file',
    (tester) async {
      usePicker(folder);
      final opened = <String>[];
      final fake = FakePeerBeam()
        ..updateCheck = available
        ..updateDownload = saved;
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: CheckUpdatesTile(
              api: fake,
              openFolder: (path) async {
                opened.add(path);
                return null;
              },
            ),
          ),
        ),
      );
      await check(tester);
      await tester.tap(find.text('Download'));
      await tester.pumpAndSettle();

      // Says what was written and where.
      expect(find.textContaining('peerbeam-0.9.1-amd64.deb'), findsOneWidget);
      expect(find.textContaining(folder), findsOneWidget);

      await tester.tap(find.text('Show folder'));
      await tester.pumpAndSettle();
      // The folder, exactly. Handing the platform opener the file instead would
      // pass a .deb to the system installer -- A3 condition 4.
      expect(opened, [folder]);
    },
    variant: TargetPlatformVariant.desktop(),
  );

  testWidgets(
    'a refused download says why, and offers no folder',
    (tester) async {
      usePicker(folder);
      final fake = FakePeerBeam()
        ..updateCheck = available
        ..updateDownload = const UpdateDownload(
          ok: false,
          downloaded: false,
          current: '0.9.0',
          latest: '0.9.1',
          reason: "the checksums are not signed by this project's key",
        );
      await openSettings(tester, fake);
      await check(tester);
      await tester.tap(find.text('Download'));
      await tester.pumpAndSettle();

      expect(
        find.textContaining(
          "the checksums are not signed by this project's key",
        ),
        findsOneWidget,
      );
      expect(find.text('Show folder'), findsNothing);
    },
    variant: TargetPlatformVariant.desktop(),
  );

  testWidgets(
    'a download that fails outright is reported in place',
    (tester) async {
      usePicker(folder);
      final fake = FakePeerBeam()
        ..updateCheck = available
        ..failing.add('downloadUpdate');
      await openSettings(tester, fake);
      await check(tester);
      await tester.tap(find.text('Download'));
      await tester.pumpAndSettle();

      expect(find.textContaining('Could not download'), findsOneWidget);
      expect(find.byType(CircularProgressIndicator), findsNothing);
      expect(find.text('Show folder'), findsNothing);
    },
    variant: TargetPlatformVariant.desktop(),
  );

  testWidgets('a phone is offered no download', (tester) async {
    final fake = FakePeerBeam()..updateCheck = available;
    await openSettings(tester, fake);
    await check(tester);
    // There is no artifact to fetch here: Android updates through its own
    // install, and the engine would only refuse.
    expect(find.text('Download'), findsNothing);
  }, variant: TargetPlatformVariant.mobile());
}

class _Picker extends FileSelectorPlatform {
  _Picker(this.answer);

  final String? answer;
  int asked = 0;

  @override
  Future<String?> getDirectoryPath({
    String? initialDirectory,
    String? confirmButtonText,
  }) async {
    asked++;
    return answer;
  }
}
