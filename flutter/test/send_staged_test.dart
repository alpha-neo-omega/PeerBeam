import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/features/send/send_staged.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/state/app_scope.dart';
import 'package:peerbeam/state/staging.dart';
import 'package:peerbeam/state/stores.dart';
import 'sdk/fake_peerbeam.dart';

void main() {
  testWidgets('sendStaged batches files + materialized text and streams folders', (
    tester,
  ) async {
    final fake = FakePeerBeam();
    final state = AppState.live(fake);
    state.staging.add([
      StagedFile(path: '/x/a.bin', name: 'a.bin', size: 10),
      StagedFile(path: '/x/dir', name: 'dir', size: 0, isDirectory: true),
    ]);
    state.staging.addText('hello world');

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

    await sendStaged(
      ctx,
      const PeerTarget(name: 'Laptop', addresses: ['host'], port: 49600),
      'Laptop',
    );
    await tester.pump();

    // One batch send with the file + a materialized clipboard payload.
    final sendCall = fake.calls.firstWhere(
      (c) => c.startsWith('send:'),
      orElse: () => '',
    );
    expect(sendCall, contains('/x/a.bin'));
    expect(sendCall, contains('peerbeam-clipboard-'));
    // Folder streamed on its own.
    expect(fake.calls, contains('sendFolder:/x/dir'));
    // Stack cleared on success.
    expect(state.staging.isEmpty, isTrue);
  });
}
