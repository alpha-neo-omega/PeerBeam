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
}
