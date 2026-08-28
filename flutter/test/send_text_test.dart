import 'dart:io';

import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/features/send/send_text.dart';

/// Stand in for `path_provider`, which has no implementation in a unit test.
///
/// Every test here needs it because `writeTextPayload` asks the platform where
/// it may write rather than using `dart:io`'s system temp directory — on Android
/// that directory is outside the app sandbox, the write threw
/// `PathAccessException`, and since a send stages every item before sending any,
/// one staged text failed the whole batch.
Directory _stubTempDir() {
  final dir = Directory.systemTemp.createTempSync('pb-platform-temp');
  TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
      .setMockMethodCallHandler(
        const MethodChannel('plugins.flutter.io/path_provider'),
        (call) async =>
            call.method == 'getTemporaryDirectory' ? dir.path : null,
      );
  return dir;
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  late Directory temp;

  setUp(() {
    temp = _stubTempDir();
  });

  tearDown(() {
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(
          const MethodChannel('plugins.flutter.io/path_provider'),
          null,
        );
    if (temp.existsSync()) temp.deleteSync(recursive: true);
  });

  test(
    'writeTextPayload writes wire-convention file with the content',
    () async {
      final path = await writeTextPayload('hello there');
      final f = File(path);
      expect(await f.exists(), isTrue);
      expect(await f.readAsString(), 'hello there');
      expect(messageFileName.hasMatch(f.uri.pathSegments.last), isTrue);
    },
  );

  /// **Where** it writes is the fix, not merely that it writes.
  test('writeTextPayload writes where the platform says, not to systemTemp', () async {
    final path = await writeTextPayload('sandboxed');
    expect(
      path.startsWith(temp.path),
      isTrue,
      reason: 'wrote to $path instead of the directory the platform gave',
    );
    expect(await File(path).readAsString(), 'sandboxed');
  });

  test('writeTextPayload yields unique paths for back-to-back calls', () async {
    final a = await writeTextPayload('one');
    final b = await writeTextPayload('two');
    expect(a == b, isFalse);
  });

  group('a message payload is bounded before it is read', () {
    /// **The name is the peer's.** Whether a received file is shown as a
    /// message is decided purely by `messageFileName` matching, and the
    /// receiver only strips directory components from the name the sender
    /// chose — so a peer can send an arbitrarily large file called
    /// `peerbeam-clipboard-1.txt`. History used to `readAsString` it whole.
    test('an oversized payload reads as null rather than being loaded', () async {
      final dir = await Directory.systemTemp.createTemp('pb-msg');
      addTearDown(() => dir.delete(recursive: true));
      final f = File('${dir.path}/peerbeam-clipboard-1.txt');
      await f.writeAsBytes(List.filled(maxMessageBytes + 1, 0x41));

      expect(
        messageFileName.hasMatch(f.uri.pathSegments.last),
        isTrue,
        reason: 'it still looks like a message — that is the whole problem',
      );
      expect(await readMessagePayload(f.path), isNull);
    });

    test('an ordinary payload still reads', () async {
      final dir = await Directory.systemTemp.createTemp('pb-msg');
      addTearDown(() => dir.delete(recursive: true));
      final f = File('${dir.path}/peerbeam-clipboard-2.txt');
      await f.writeAsString('hello');
      expect(await readMessagePayload(f.path), 'hello');
    });

    test('a missing payload is null, not a throw', () async {
      expect(await readMessagePayload('/nowhere/peerbeam-clipboard-3.txt'), isNull);
    });
  });
}
