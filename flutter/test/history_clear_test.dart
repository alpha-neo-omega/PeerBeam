// Clearing history is a promise, and a promise the engine could not keep must
// not be reported as kept.
//
// The repository used to fire `historyClear()` and swallow its failure
// (`unawaited(... .catchError((_) {}))`), then empty the local list
// unconditionally. So a persist that failed left the screen looking cleared
// while the rows came back on the next start — and nothing, at any layer, knew.

import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/data/history_repository.dart';

import 'sdk/fake_peerbeam.dart';

void main() {
  test('a clear that the engine refused is reported', () async {
    final fake = FakePeerBeam()..failing.add('historyClear');
    final repo = HistoryRepository(api: fake);
    addTearDown(repo.dispose);

    final failure = await repo.clear();
    expect(
      failure,
      isNotNull,
      reason: 'the caller must be able to tell the user it is still on disk',
    );
  });

  test('a clear that worked reports nothing to say', () async {
    final repo = HistoryRepository(api: FakePeerBeam());
    addTearDown(repo.dispose);
    expect(await repo.clear(), isNull);
  });
}
