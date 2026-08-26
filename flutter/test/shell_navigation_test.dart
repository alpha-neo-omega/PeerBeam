import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:go_router/go_router.dart';
import 'package:peerbeam/app/shell.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'sdk/fake_peerbeam.dart';

/// **A destination must leave a pushed screen.** Chat detail, Notes and the
/// timeline are pushed onto the *branch's* navigator rather than being branches
/// of their own, and `goBranch`'s `initialLocation` resets a branch's
/// declarative location while leaving an imperatively pushed route on top of
/// it. So tapping a destination changed a screen underneath the one on display
/// and read as doing nothing, until the user pressed back.
///
/// Driven through a real `StatefulShellRoute` because that is the part that was
/// wrong: the first fix popped the root navigator, which is not where a route
/// pushed from inside a branch goes, and nothing about the code said so.
void main() {
  testWidgets('tapping a destination drops a screen pushed inside the branch', (
    tester,
  ) async {
    final router = GoRouter(
      initialLocation: '/one',
      routes: [
        StatefulShellRoute.indexedStack(
          builder: (context, state, shell) => AppShell(navigationShell: shell),
          branches: [
            StatefulShellBranch(
              routes: [
                GoRoute(
                  path: '/one',
                  builder: (c, s) => Scaffold(
                    body: Center(
                      child: TextButton(
                        onPressed: () => Navigator.of(c).push(
                          MaterialPageRoute<void>(
                            builder: (_) => const Scaffold(
                              body: Center(child: Text('DETAIL')),
                            ),
                          ),
                        ),
                        child: const Text('open detail'),
                      ),
                    ),
                  ),
                ),
              ],
            ),
            StatefulShellBranch(
              routes: [
                GoRoute(
                  path: '/two',
                  builder: (c, s) =>
                      const Scaffold(body: Center(child: Text('SECOND'))),
                ),
              ],
            ),
          ],
        ),
      ],
    );
    addTearDown(router.dispose);

    // Wide enough for the rail, so the destination is a labelled target rather
    // than depending on which icon the compact bar happens to use.
    tester.view.physicalSize = const Size(1400, 900);
    tester.view.devicePixelRatio = 1.0;
    addTearDown(tester.view.reset);

    SharedPreferences.setMockInitialValues({});
    final state = AppState.live(FakePeerBeam());
    addTearDown(state.dispose);

    await tester.pumpWidget(
      AppScope(
        state: state,
        child: MaterialApp.router(routerConfig: router),
      ),
    );
    await tester.pumpAndSettle();

    await tester.tap(find.text('open detail'));
    await tester.pumpAndSettle();
    expect(find.text('DETAIL'), findsOneWidget);

    // **Tap the branch it is already on**, which is the reported case: opening
    // chat from Home and then pressing Home. Tapping a *different* destination
    // would prove nothing — the shell is an `IndexedStack`, so the whole branch
    // is hidden either way and the pushed route would seem to vanish while
    // still sitting there, waiting to reappear.
    await tester.tap(find.text('Home').first);
    await tester.pumpAndSettle();

    expect(
      find.text('DETAIL'),
      findsNothing,
      reason: 'the pushed screen survived tapping the destination it sits in',
    );
    expect(
      find.text('open detail'),
      findsOneWidget,
      reason: 'and the branch is back at its own first screen',
    );
  });
}
