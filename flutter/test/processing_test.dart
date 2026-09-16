// The blocking spinner: it must come down, and it must take only itself.
//
// `withProcessing` covers an operation with no other visible feedback — a large
// picked file being streamed into app storage, a dial that asks an address who
// is there. Its barrier is `PopScope(canPop: false)` with
// `barrierDismissible: false`, which is the right call for an operation that
// must not be interrupted and also the reason every way out of it has to work:
// a spinner that fails to dismiss is an app nobody can use again.

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/widgets/processing.dart';

/// A button that runs [action] under the spinner and then, optionally, opens a
/// dialog of its own — the sequence that used to eat the second dialog.
Widget _host({
  required Future<String> Function() action,
  bool thenDialog = false,
  void Function(String)? onDone,
}) => MaterialApp(
  home: Builder(
    builder: (ctx) => Scaffold(
      body: Center(
        child: TextButton(
          onPressed: () async {
            final result = await withProcessing(ctx, 'working…', action);
            onDone?.call(result);
            if (thenDialog && ctx.mounted) {
              await showDialog<void>(
                context: ctx,
                builder: (_) => const AlertDialog(title: Text('mine')),
              );
            }
          },
          child: const Text('go'),
        ),
      ),
    ),
  ),
);

/// Pump by hand rather than settling: the spinner animates forever, so
/// `pumpAndSettle` would time out on the state being waited through.
Future<void> _pumpFrames(WidgetTester tester, [int n = 8]) async {
  for (var i = 0; i < n; i++) {
    await tester.pump(const Duration(milliseconds: 200));
  }
}

void main() {
  testWidgets('the spinner goes up and comes down again', (tester) async {
    final gate = Completer<String>();
    await tester.pumpWidget(_host(action: () => gate.future));

    await tester.tap(find.text('go'));
    await tester.pump();
    expect(find.text('working…'), findsOneWidget);

    gate.complete('done');
    await _pumpFrames(tester);
    expect(find.text('working…'), findsNothing);
  });

  // The defect. An action that finishes inside the frame the spinner was
  // requested in reaches the `finally` before the dialog's builder has run, so
  // the dismissal is booked for the next frame — by which time the caller has
  // opened its own dialog on top. `Navigator.pop()` removes the route on top,
  // not the one it belongs to, so it took the caller's dialog and left the
  // spinner up for good: an app wedged behind a barrier that cannot be
  // dismissed, which is exactly what the post-frame retry was added to prevent.
  testWidgets('an instant action does not eat the dialog opened after it', (
    tester,
  ) async {
    await tester.pumpWidget(
      _host(action: () async => 'instant', thenDialog: true),
    );

    await tester.tap(find.text('go'));
    await _pumpFrames(tester);

    expect(find.text('working…'), findsNothing, reason: 'spinner came down');
    expect(find.text('mine'), findsOneWidget, reason: "caller's dialog stands");
  });

  testWidgets('a slow action does not eat it either', (tester) async {
    final gate = Completer<String>();
    await tester.pumpWidget(_host(action: () => gate.future, thenDialog: true));

    await tester.tap(find.text('go'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 300)); // builder has run
    gate.complete('slow');
    await _pumpFrames(tester);

    expect(find.text('working…'), findsNothing);
    expect(find.text('mine'), findsOneWidget);
  });

  // The value still has to get back to the caller, whichever path dismissed it.
  testWidgets('the action result is returned', (tester) async {
    String? got;
    await tester.pumpWidget(
      _host(action: () async => 'value', onDone: (v) => got = v),
    );
    await tester.tap(find.text('go'));
    await _pumpFrames(tester);

    expect(got, 'value');
  });

  // A failure must not leave the barrier up — that is the one state with no
  // way out.
  testWidgets('a throwing action still takes the spinner down', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (ctx) => Scaffold(
            body: Center(
              child: TextButton(
                onPressed: () async {
                  try {
                    await withProcessing(
                      ctx,
                      'working…',
                      () async => throw StateError('nope'),
                    );
                  } catch (_) {
                    // The caller's problem; the spinner is this file's.
                  }
                },
                child: const Text('go'),
              ),
            ),
          ),
        ),
      ),
    );

    await tester.tap(find.text('go'));
    await _pumpFrames(tester);
    expect(find.text('working…'), findsNothing);
  });
}
