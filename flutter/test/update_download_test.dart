import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/models.dart';

/// Decoding what `pb_download_update` answers and what it reports while it
/// runs. The shape mirrors `peerbeam download-update --json`, so the two
/// surfaces describe one download the same way.
void main() {
  group('UpdateDownload.fromJson', () {
    // The break this catches: a reply that does not say a file was written,
    // read as though it did. The tile would then tell someone a verified file
    // is waiting on disk when nothing was written at all.
    test(
      'a reply that does not say a file was written is not taken as one',
      () {
        final d = UpdateDownload.fromJson(const {'current': '0.12.1'});
        expect(d.downloaded, isFalse);
        expect(d.ok, isFalse);
        expect(d.path, isNull);
        expect(d.current, '0.12.1');
      },
    );

    test('a written file carries where it is and what it is', () {
      final d = UpdateDownload.fromJson(const {
        'ok': true,
        'downloaded': true,
        'current': '0.12.1',
        'latest': '0.12.2',
        'path': '/home/me/Downloads/peerbeam-0.12.2-amd64.deb',
        'name': 'peerbeam-0.12.2-amd64.deb',
        'bytes': 12901854,
      });
      expect(d.ok, isTrue);
      expect(d.downloaded, isTrue);
      expect(d.latest, '0.12.2');
      expect(d.path, '/home/me/Downloads/peerbeam-0.12.2-amd64.deb');
      expect(d.name, 'peerbeam-0.12.2-amd64.deb');
      expect(d.bytes, 12901854);
      expect(d.reason, isNull);
    });

    test('a refusal carries its reason', () {
      final d = UpdateDownload.fromJson(const {
        'ok': false,
        'downloaded': false,
        'current': '0.12.1',
        'latest': '0.12.2',
        'reason': "the checksums are not signed by this project's key",
      });
      expect(d.downloaded, isFalse);
      expect(d.reason, "the checksums are not signed by this project's key");
    });
  });

  group('update_download_progress', () {
    test('decodes to progress the tile can show', () {
      final e = BridgeEvent.fromJson(const {
        'type': 'update_download_progress',
        'done': 5242880,
        'total': 10485760,
      });
      expect(e, isA<UpdateDownloadProgress>());
      final p = e! as UpdateDownloadProgress;
      expect(p.done, 5242880);
      expect(p.total, 10485760);
    });

    // A server need not send a length. The progress must still decode -- as
    // "this much so far" -- rather than as a total of zero, which reads as
    // either 0% for ever or a division by zero.
    test('an unknown total decodes as null, not zero', () {
      final e = BridgeEvent.fromJson(const {
        'type': 'update_download_progress',
        'done': 100,
      });
      final p = e! as UpdateDownloadProgress;
      expect(p.done, 100);
      expect(p.total, isNull);
    });
  });
}
