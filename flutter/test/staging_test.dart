import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/state/staging.dart';

StagedFile file(String path, int size, {bool dir = false}) => StagedFile(
  path: path,
  name: path.split('/').last,
  size: size,
  isDirectory: dir,
);

void main() {
  group('StagingStore', () {
    test('adds files and reports count + total (large sizes)', () {
      final s = StagingStore();
      final added = s.add([
        file('/a/big.iso', 8 * 1024 * 1024 * 1024), // 8 GiB
        file('/a/clip.mov', 2 * 1024 * 1024 * 1024), // 2 GiB
      ]);
      expect(added, 2);
      expect(s.count, 2);
      expect(s.totalBytes, 10 * 1024 * 1024 * 1024);
      expect(s.isEmpty, isFalse);
    });

    test('deduplicates by path', () {
      final s = StagingStore();
      s.add([file('/x/one.bin', 10)]);
      final added = s.add([
        file('/x/one.bin', 10), // duplicate path
        file('/x/two.bin', 20),
      ]);
      expect(added, 1);
      expect(s.count, 2);
    });

    test('handles many files', () {
      final s = StagingStore();
      final many = List.generate(500, (i) => file('/bulk/f$i.dat', i));
      expect(s.add(many), 500);
      expect(s.count, 500);
    });

    test('remove and clear', () {
      final s = StagingStore();
      s.add([file('/a', 1), file('/b', 2)]);
      s.remove('/a');
      expect(s.count, 1);
      expect(s.items.single.path, '/b');
      s.clear();
      expect(s.isEmpty, isTrue);
      expect(s.totalBytes, 0);
    });

    test('notifies listeners only on real change', () {
      final s = StagingStore();
      var notes = 0;
      s.addListener(() => notes++);

      s.add([file('/a', 1)]);
      expect(notes, 1);
      s.add([file('/a', 1)]); // duplicate → no notify
      expect(notes, 1);
      s.remove('/missing'); // no-op → no notify
      expect(notes, 1);
      s.remove('/a');
      expect(notes, 2);
    });

    test('text items are distinct, not deduped, and sized by content', () {
      final s = StagingStore();
      final a = s.addText('hello');
      final b = s.addText('hello'); // identical content is still a new item
      expect(s.count, 2);
      expect(a.id == b.id, isFalse);
      expect(a.isText, isTrue);
      expect(a.kind, StagedKind.text);
      expect(a.size, 5); // 'hello' = 5 UTF-8 bytes
    });

    test('totalBytes includes text byte length', () {
      final s = StagingStore();
      s.add([file('/a', 100)]);
      s.addText('abc'); // 3 bytes
      expect(s.totalBytes, 103);
    });

    test('remove by id removes a text item', () {
      final s = StagingStore();
      final t = s.addText('bye');
      expect(s.count, 1);
      s.remove(t.id);
      expect(s.isEmpty, isTrue);
    });

    test('preview truncates a long single line', () {
      final s = StagingStore();
      final t = s.addText('x' * 200);
      expect(t.preview.endsWith('…'), isTrue);
      expect(t.preview.length, lessThanOrEqualTo(81));
    });
  });
}
