import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/features/send/send_text.dart';

void main() {
  test('writeTextPayload writes wire-convention file with the content', () async {
    final path = await writeTextPayload('hello there');
    final f = File(path);
    expect(await f.exists(), isTrue);
    expect(await f.readAsString(), 'hello there');
    expect(messageFileName.hasMatch(f.uri.pathSegments.last), isTrue);
  });

  test('writeTextPayload yields unique paths for back-to-back calls', () async {
    final a = await writeTextPayload('one');
    final b = await writeTextPayload('two');
    expect(a == b, isFalse);
  });
}
