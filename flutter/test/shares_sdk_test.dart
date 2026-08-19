import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/sdk/models.dart';

import 'sdk/fake_peerbeam.dart';

void main() {
  test('a shared folder carries the path, not just the name', () {
    // Two folders called `Documents` are indistinguishable by name, and nobody
    // should confirm a share they cannot identify.
    final f = SharedFolder.fromJson(const {
      'name': 'Documents',
      'path': '/home/me/work/Documents',
      'exists': true,
    });
    expect(f.name, 'Documents');
    expect(f.path, '/home/me/work/Documents');
    expect(f.exists, isTrue);
  });

  /// A share whose folder has been moved or deleted must still be listed:
  /// silently dropping it leaves someone believing they share something they
  /// do not.
  test('a share whose folder is gone is reported, not hidden', () {
    final f = SharedFolder.fromJson(const {
      'name': 'Old',
      'path': '/gone',
      'exists': false,
    });
    expect(f.exists, isFalse);
    expect(f.path, '/gone');
  });

  test('a malformed entry decodes rather than throwing', () {
    final f = SharedFolder.fromJson(const {});
    expect(f.name, isEmpty);
    expect(f.exists, isFalse);
  });

  test('nothing is shared until it is chosen', () async {
    final api = FakePeerBeam();
    expect(await api.sharedFolders(), isEmpty);
  });

  test('setting shares reaches the engine and is readable back', () async {
    final api = FakePeerBeam();
    await api.setSharedFolders(['/home/me/Photos', '/srv/media']);

    expect(api.calls, contains('setSharedFolders:/home/me/Photos,/srv/media'));
    final shares = await api.sharedFolders();
    expect(shares.map((s) => s.path), ['/home/me/Photos', '/srv/media']);
    expect(shares.first.name, 'Photos');
  });

  test('un-sharing everything leaves nothing shared', () async {
    final api = FakePeerBeam();
    await api.setSharedFolders(['/home/me/Private']);
    expect(await api.sharedFolders(), hasLength(1));

    await api.setSharedFolders([]);
    expect(
      await api.sharedFolders(),
      isEmpty,
      reason: 'un-sharing must actually un-share',
    );
  });
}
