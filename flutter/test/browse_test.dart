// Not a new dependency: file_selector_platform_interface already resolves
// transitively through file_selector (see pubspec.lock). Reached into here,
// test-only, to stand in for the directory picker the sync flow opens — the
// same seam file_selector_linux/macos/windows each implement for real.
// ignore: depend_on_referenced_packages
import 'package:file_selector_platform_interface/file_selector_platform_interface.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/browse/browse_screen.dart';
import 'package:peerbeam/sdk/exceptions.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

/// Answers the "sync into where?" directory picker with a fixed path, so the
/// sync itself is what the test is about.
class _FakeDirectoryPicker extends FileSelectorPlatform {
  @override
  Future<String?> getDirectoryPath({
    String? initialDirectory,
    String? confirmButtonText,
  }) async => '/home/me/synced';
}

const _peer = PeerTarget(
  id: 'pb-bob',
  name: 'Bob',
  addresses: ['127.0.0.1'],
  port: 49600,
);

Future<void> _open(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: const MaterialApp(home: BrowseScreen(peer: _peer)),
    ),
  );
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 300));
}

void main() {
  testWidgets('a denied listing names every possible reason, not one', (
    tester,
  ) async {
    // The device sends one answer for every reason. Naming a single cause
    // would invent information the protocol deliberately withholds.
    final fake = FakePeerBeam()..browseDenied = true;
    await _open(tester, fake);

    expect(find.text('Nothing to show'), findsOneWidget);
    expect(find.textContaining('may not share anything here'), findsOneWidget);
    expect(find.textContaining('permission'), findsOneWidget);
  });

  testWidgets('shares are listed and a folder can be opened', (tester) async {
    final fake = FakePeerBeam()
      ..shared[''] = [const BrowseEntry(name: 'photos', isDir: true, size: 0)]
      ..shared['photos'] = [
        const BrowseEntry(name: 'holiday.jpg', isDir: false, size: 2048),
      ];
    await _open(tester, fake);

    expect(find.text('photos'), findsOneWidget);
    await tester.tap(find.text('photos'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.text('holiday.jpg'), findsOneWidget);
    expect(find.text('2.0 KB'), findsOneWidget);
    expect(fake.calls, contains('browse:pb-bob:photos'));
  });

  testWidgets('the path shown is share-relative, never a filesystem location', (
    tester,
  ) async {
    // The wire carries share-relative paths precisely so a device's real layout
    // stays its own business; the UI must not reintroduce one.
    final fake = FakePeerBeam()
      ..shared[''] = [const BrowseEntry(name: 'docs', isDir: true, size: 0)]
      ..shared['docs'] = [
        const BrowseEntry(name: 'a.txt', isDir: false, size: 1),
      ];
    await _open(tester, fake);
    await tester.tap(find.text('docs'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    expect(find.text('docs'), findsOneWidget);
    expect(find.textContaining('/home/'), findsNothing);
    expect(find.textContaining('/Users/'), findsNothing);
  });

  /// **Syncing is offered inside a share, not at the top of one.** At the top
  /// level there is no single folder to sync, and a button there would promise
  /// "everything they share" — a much larger commitment than it can keep.
  testWidgets('the sync action appears only once inside a folder', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    await _open(tester, fake);

    expect(
      find.byIcon(Icons.sync),
      findsNothing,
      reason: 'the share list is not a folder to sync',
    );
  });

  /// **A failure is reported in the app's own words, never the engine's.**
  ///
  /// This snackbar interpolated the raw exception — the one thing
  /// `sdk/error_text.dart` exists to keep off screen. A peer that has gone to
  /// sleep is the ordinary way a sync fails, and "Sync failed:
  /// ConnectionException: quic: timed out" tells the person holding the phone
  /// nothing they can act on, while naming internals to anyone they show it to.
  testWidgets('a failed sync is reported in the app\'s own words', (
    tester,
  ) async {
    // Restored afterwards: the instance is process-global, so a fake left
    // behind goes on answering for every later test in this process.
    final realPicker = FileSelectorPlatform.instance;
    FileSelectorPlatform.instance = _FakeDirectoryPicker();
    addTearDown(() => FileSelectorPlatform.instance = realPicker);

    final fake = FakePeerBeam()
      ..shared[''] = [const BrowseEntry(name: 'photos', isDir: true, size: 0)]
      ..shared['photos'] = const []
      ..syncError = const ConnectionException('quic handshake timed out');
    await _open(tester, fake);

    // Sync is offered inside a folder only, so open one first.
    await tester.tap(find.text('photos'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300));

    await tester.tap(find.byIcon(Icons.sync));
    await tester.pumpAndSettle();

    expect(fake.calls, contains('syncFolder:pb-bob:photos:/home/me/synced'));
    expect(
      find.textContaining("Couldn't reach the device"),
      findsOneWidget,
      reason: 'the friendly sentence for a connection failure',
    );
    expect(
      find.textContaining('ConnectionException'),
      findsNothing,
      reason: 'the exception type is not something a user can act on',
    );
    expect(find.textContaining('quic handshake timed out'), findsNothing);
  });
}
