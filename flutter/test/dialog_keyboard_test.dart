// Dialogs that ask for typing, with the keyboard actually up.
//
// Every dialog in this app that holds a `TextField` is only ever read with a
// keyboard covering the bottom half of the screen — that is what typing into
// one means. `AlertDialog` handles its half of that: it shrinks itself into the
// space above the inset. What it cannot do is shrink the `content` widget the
// caller handed it, so an unscrolled `Column` of fields in there overflows, and
// the part that goes missing is the bottom — the field being filled in, or the
// line explaining what was wrong with it.
//
// The three dialogs below are the ones with enough in them to hit that: the
// note editor (title + body), and Add/Edit device (name, host, port, plus a
// validation line). Each is opened at the narrowest supported layout with a
// keyboard raised over it, and each must render whole.

import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'package:peerbeam/features/home/home_screen.dart';
import 'package:peerbeam/features/notes/notes_screen.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

/// A 360x720 phone — the narrowest layout the app supports, and the one a
/// keyboard leaves least of.
void _phone(WidgetTester tester) {
  tester.view.physicalSize = const Size(360, 720);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);
}

/// Raise a keyboard over whatever is on screen.
///
/// 400 logical pixels of a 720px screen, which is an ordinary Android soft
/// keyboard once its suggestion strip is counted. Applied after the dialog is
/// open, because that is the order it happens in: the dialog appears, its field
/// takes focus, and the keyboard comes up over the top of it.
Future<void> _raiseKeyboard(WidgetTester tester) async {
  tester.view.viewInsets = const FakeViewPadding(bottom: 400);
  await tester.pumpAndSettle();
}

Future<AppState> _openNotes(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: const MaterialApp(home: NotesScreen()),
    ),
  );
  await tester.pump();
  await tester.pump(const Duration(milliseconds: 300));
  return state;
}

Future<AppState> _openHome(WidgetTester tester, FakePeerBeam fake) async {
  final state = AppState.live(fake);
  addTearDown(state.dispose);
  await tester.pumpWidget(
    AppScope(
      state: state,
      child: const MaterialApp(home: HomeScreen()),
    ),
  );
  await tester.pump();
  await state.saved.load();
  await tester.pumpAndSettle();
  return state;
}

void main() {
  setUp(() => SharedPreferences.setMockInitialValues({}));

  /// The note editor: a title field and a body field that grows to ten lines.
  testWidgets('the note editor fits above a keyboard on a phone', (
    tester,
  ) async {
    _phone(tester);
    await _openNotes(tester, FakePeerBeam());

    await tester.tap(find.byIcon(Icons.add_rounded));
    await tester.pumpAndSettle();
    await _raiseKeyboard(tester);

    expect(
      tester.takeException(),
      isNull,
      reason: 'the editor overflowed instead of scrolling',
    );
    // Still usable, not merely non-throwing: the body field and the button that
    // commits it both have to be reachable.
    expect(find.text('New note'), findsOneWidget);
    expect(find.text('Save'), findsOneWidget);
  });

  /// Add device: three fields, and after a bad port a fourth line of copy.
  testWidgets('the Add device dialog fits above a keyboard on a phone', (
    tester,
  ) async {
    _phone(tester);
    await _openHome(tester, FakePeerBeam());

    await tester.ensureVisible(find.byTooltip('Add device by address'));
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('Add device by address'));
    await tester.pumpAndSettle();
    await _raiseKeyboard(tester);

    expect(
      tester.takeException(),
      isNull,
      reason: 'the dialog overflowed instead of scrolling',
    );
    expect(find.text('Add device'), findsOneWidget);
  });

  /// The same dialog carrying the validation line, which is the tallest this
  /// content ever gets — and the state in which the missing part matters most,
  /// since the line says what to fix.
  testWidgets('the Add device dialog still fits once it is showing an error', (
    tester,
  ) async {
    _phone(tester);
    await _openHome(tester, FakePeerBeam());

    await tester.ensureVisible(find.byTooltip('Add device by address'));
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('Add device by address'));
    await tester.pumpAndSettle();
    await _raiseKeyboard(tester);

    // Saving with an empty host is what puts the error line on screen.
    await tester.tap(find.text('Save'));
    await tester.pumpAndSettle();

    expect(tester.takeException(), isNull);
    expect(
      find.text('Enter a host and a port between 1 and 65535'),
      findsOneWidget,
      reason: 'the line naming what to fix is the one that must not fall off',
    );
  });

  /// Edit device: the same three fields, reached through a saved entry.
  testWidgets('the Edit device dialog fits above a keyboard on a phone', (
    tester,
  ) async {
    _phone(tester);
    SharedPreferences.setMockInitialValues({
      'saved_devices_v1': jsonEncode([
        {'id': 's1', 'name': 'Server', 'host': '10.0.0.4', 'port': 49600},
      ]),
    });
    await _openHome(tester, FakePeerBeam());

    await tester.ensureVisible(find.byTooltip('Device actions'));
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('Device actions'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Edit'));
    await tester.pumpAndSettle();
    await _raiseKeyboard(tester);

    expect(
      tester.takeException(),
      isNull,
      reason: 'the dialog overflowed instead of scrolling',
    );
    expect(find.text('Edit device'), findsOneWidget);
  });
}
