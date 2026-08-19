import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/features/notes/notes_screen.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

Widget _screen(AppState state) => AppScope(
  state: state,
  child: const MaterialApp(home: NotesScreen()),
);

Future<AppState> _open(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(_screen(state));
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 300));
  return state;
}

void main() {
  testWidgets('an empty store shows the empty state, not a blank screen', (
    tester,
  ) async {
    await _open(tester, FakePeerBeam());
    expect(find.text('No notes yet'), findsOneWidget);
  });

  testWidgets('a note shows its title, and its first line when it has none', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    await fake.notesCreate('milk\nbread', title: 'Shopping');
    await fake.notesCreate('no heading here\nsecond line');
    await _open(tester, fake);

    expect(find.text('Shopping'), findsOneWidget);
    // A note with no title is headed by its first line rather than by
    // "Untitled", which would tell the reader nothing.
    expect(find.text('no heading here'), findsOneWidget);
  });

  testWidgets('writing a note saves it', (tester) async {
    final fake = FakePeerBeam();
    await _open(tester, fake);

    await tester.tap(find.byIcon(Icons.add_rounded));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField).last, 'remember this');
    await tester.pump();
    await tester.tap(find.text('Save'));
    await tester.pumpAndSettle();

    expect(fake.calls, contains('notesCreate:'));
    expect(fake.notes.single.body, 'remember this');
  });

  testWidgets('an empty note cannot be saved', (tester) async {
    // "Untitled" with nothing under it is not a note, and the list would have
    // no way to show what it is.
    final fake = FakePeerBeam();
    await _open(tester, fake);

    await tester.tap(find.byIcon(Icons.add_rounded));
    await tester.pumpAndSettle();

    final save = tester.widget<FilledButton>(
      find.ancestor(of: find.text('Save'), matching: find.byType(FilledButton)),
    );
    expect(save.onPressed, isNull, reason: 'Save was enabled with no text');
  });

  testWidgets('editing a note the engine has deleted says so', (tester) async {
    // The engine refuses to resurrect a tombstone, so the edit genuinely did
    // not land. Silence would leave the user believing it had.
    final fake = FakePeerBeam();
    final id = await fake.notesCreate('temporary');
    await _open(tester, fake);

    // Deleted behind the screen's back, as another device would.
    await fake.notesDelete(id);

    await tester.tap(find.text('temporary').first);
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField).last, 'back again');
    await tester.pump();
    await tester.tap(find.text('Save'));
    await tester.pumpAndSettle();

    expect(
      find.text('That note was deleted, so the edit was not saved.'),
      findsOneWidget,
    );
  });

  testWidgets('deleting asks first, and only then deletes', (tester) async {
    final fake = FakePeerBeam();
    await fake.notesCreate('goodbye');
    await _open(tester, fake);

    await tester.tap(find.byIcon(Icons.delete_outline_rounded));
    await tester.pumpAndSettle();
    expect(find.text('Delete note?'), findsOneWidget);

    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(
      fake.calls.where((c) => c.startsWith('notesDelete')),
      isEmpty,
      reason: 'cancelling still deleted the note',
    );

    await tester.tap(find.byIcon(Icons.delete_outline_rounded));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Delete'));
    await tester.pumpAndSettle();
    expect(fake.calls.any((c) => c.startsWith('notesDelete')), isTrue);
  });

  testWidgets('sync offers only devices actually granted the permission', (
    tester,
  ) async {
    // Offering a device that would be refused turns a permission the user set
    // into a failure they have to diagnose.
    final fake = FakePeerBeam()
      ..trusted.addAll([
        TrustedDevice(
          id: 'pb-yes',
          name: 'Laptop',
          fingerprint: 'aa',
          trustedAt: DateTime.now(),
          approved: true,
          permissions: const {'files', 'notes'},
        ),
        TrustedDevice(
          id: 'pb-no',
          name: 'Phone',
          fingerprint: 'bb',
          trustedAt: DateTime.now(),
          approved: true,
          permissions: const {'files'},
        ),
      ]);
    final state = await _open(tester, fake);
    await state.trust.refresh();
    await tester.pump();

    await tester.tap(find.byIcon(Icons.sync_rounded));
    await tester.pumpAndSettle();

    expect(find.text('Laptop'), findsOneWidget);
    expect(
      find.text('Phone'),
      findsNothing,
      reason: 'a device without the notes permission was offered',
    );
  });

  testWidgets('sync says so when no device may sync notes', (tester) async {
    final fake = FakePeerBeam();
    await _open(tester, fake);

    await tester.tap(find.byIcon(Icons.sync_rounded));
    await tester.pumpAndSettle();

    expect(find.textContaining('No device may sync notes yet'), findsOneWidget);
  });
}
