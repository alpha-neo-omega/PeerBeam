// "Not read yet" and "nothing to show" are different facts.
//
// No repository except Notes carried a loaded flag, so every screen decided
// empty-versus-list off the data alone. Every cold start therefore flashed a
// confident falsehood — "Nothing here yet", "No active transfers" — before the
// first read answered. A user who taps in during boot concludes their data is
// gone, and on the transfers screen the row that matters most is precisely the
// one a restart interrupted.

import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/data/history_repository.dart';
import 'package:peerbeam/data/transfer_repository.dart';

import 'sdk/fake_peerbeam.dart';

void main() {
  group('history', () {
    test('is not "loaded" before anything has been read', () {
      expect(HistoryRepository().loaded, isFalse);
    });

    test('is loaded once the read answers', () async {
      final repo = HistoryRepository(api: FakePeerBeam());
      await repo.refresh();
      expect(repo.loaded, isTrue);
      expect(
        repo.items,
        isEmpty,
        reason: 'genuinely empty, and now known to be',
      );
    });

    test('a failed read still ends the load', () async {
      final fake = FakePeerBeam()..failing.add('history');
      final repo = HistoryRepository(api: fake);
      await repo.refresh();
      expect(
        repo.loaded,
        isTrue,
        reason: 'staying "loading" forever is the same lie told the other way',
      );
    });
  });

  group('transfers', () {
    test('is not "loaded" before anything has been read', () {
      expect(TransferRepository().loaded, isFalse);
    });

    test('is loaded once the interrupted read answers', () async {
      final repo = TransferRepository(api: FakePeerBeam());
      await repo.refreshInterrupted();
      expect(repo.loaded, isTrue);
    });

    test('a failed read still ends the load', () async {
      final fake = FakePeerBeam()..failing.add('interruptedTransfers');
      final repo = TransferRepository(api: fake);
      await repo.refreshInterrupted();
      expect(repo.loaded, isTrue);
    });
  });
}
