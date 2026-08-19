import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/features/settings/settings_screen.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/stores.dart';

import 'sdk/fake_peerbeam.dart';

/// The About section names where releases live and says, in as many words, that
/// the app will not go and look. Both halves matter: the address without the
/// sentence reads as an oversight, and the sentence without the address leaves
/// somebody with nowhere to go.
void main() {
  Future<void> pump(WidgetTester tester, FakePeerBeam fake) async {
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
      find.text('ABOUT'),
      300,
      scrollable: find.byType(Scrollable).first,
    );
    await tester.pumpAndSettle();
  }

  testWidgets('the releases address is shown', (tester) async {
    await pump(tester, FakePeerBeam());
    expect(
      find.textContaining('github.com/alpha-neo-omega/PeerBeam/releases'),
      findsOneWidget,
    );
  });

  testWidgets('it says the app does not check on its own', (tester) async {
    await pump(tester, FakePeerBeam());
    expect(
      find.textContaining('never checks for updates on its own'),
      findsOneWidget,
      reason: 'silence would read as an oversight rather than a decision',
    );
  });

  testWidgets('the address can be copied', (tester) async {
    final copied = <String>[];
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      SystemChannels.platform,
      (call) async {
        if (call.method == 'Clipboard.setData') {
          copied.add((call.arguments as Map)['text'] as String);
        }
        return null;
      },
    );
    await pump(tester, FakePeerBeam());
    await tester.tap(find.byTooltip('Copy the releases address'));
    await tester.pumpAndSettle();

    expect(copied, ['https://github.com/alpha-neo-omega/PeerBeam/releases']);
    expect(find.text('Releases address copied'), findsOneWidget);
  });
}
