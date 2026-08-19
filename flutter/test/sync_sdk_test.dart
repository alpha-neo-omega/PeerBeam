import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/sdk/models.dart';

import 'sdk/fake_peerbeam.dart';

void main() {
  test('a sync result decodes every outcome the engine reports', () {
    final r = SyncResult.fromJson(const {
      'fetching': 3,
      'pushing': 1,
      'deleted': 2,
      'renamed': 4,
      'conflicts': ['notes.sync-conflict-bob.txt'],
      'truncated': true,
    });
    expect(r.fetching, 3);
    expect(r.pushing, 1);
    expect(r.deleted, 2);
    expect(r.renamed, 4);
    expect(r.conflicts, ['notes.sync-conflict-bob.txt']);
    expect(r.truncated, isTrue);
    expect(r.isIdle, isFalse);
  });

  test('a sync with nothing to do reports itself as idle', () {
    expect(SyncResult.fromJson(const {}).isIdle, isTrue);
  });

  /// A conflict means work outstanding even when no bytes move, so it must not
  /// read as idle — that is the case where a person has to choose.
  test('conflicts alone are not idle', () {
    final r = SyncResult.fromJson(const {
      'conflicts': ['a.sync-conflict-bob.txt'],
    });
    expect(r.isIdle, isFalse);
  });

  test('a result from an older engine decodes rather than throwing', () {
    // Fields this build knows may simply be absent.
    final r = SyncResult.fromJson(const {'fetching': 2});
    expect(r.fetching, 2);
    expect(r.renamed, 0);
    expect(r.conflicts, isEmpty);
  });

  test(
    'watching a folder is reachable from the SDK and can be undone',
    () async {
      final api = FakePeerBeam();
      const peer = PeerTarget(
        id: 'pb-bob',
        name: 'Bob',
        addresses: [],
        port: 0,
      );

      expect(await api.watchedFolders(), isEmpty);
      await api.watchFolder(peer, 'share/docs', '/home/me/docs');
      final watched = await api.watchedFolders();
      expect(watched.single.path, 'share/docs');
      expect(watched.single.into, '/home/me/docs');

      await api.unwatchFolder('share/docs', '/home/me/docs');
      expect(await api.watchedFolders(), isEmpty);
    },
  );

  test(
    'syncing a folder reaches the engine with the peer and both paths',
    () async {
      final api = FakePeerBeam();
      await api.syncFolder(
        const PeerTarget(id: 'pb-bob', name: 'Bob', addresses: [], port: 0),
        'share/docs',
        '/home/me/docs',
      );
      expect(api.calls, contains('syncFolder:pb-bob:share/docs:/home/me/docs'));
    },
  );
}
