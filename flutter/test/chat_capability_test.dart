// Which devices can hold a conversation, and which cannot.
//
// A chat row is filed under `chat-<device id>`, and the engine's store rejects
// any namespace outside [A-Za-z0-9._-] (peerbeam-appstore-fs's
// `namespace_dir`). A Tailscale peer's id is `ts:<node id>`; the colon makes
// every store call for it fail with `invalid namespace`. The app used to offer
// the chat button anyway, so the thread opened, stayed empty, and swallowed
// whatever was typed into it.

import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/state/models.dart';

void main() {
  group('a Tailscale peer cannot be chatted with', () {
    // The exact id shape `peerbeam-discovery-tailscale` mints
    // (status.rs: `DeviceId::from(format!("ts:{raw_id}"))`).
    test('its ts: id is rejected, because the store rejects the colon', () {
      expect(canChatWithDeviceId('ts:nodeidabc123'), isFalse);
      expect(canChatWithDeviceId('ts:n1'), isFalse);
    });

    test('an ordinary discovered id is fine', () {
      expect(canChatWithDeviceId('pb-laptop00001'), isTrue);
      expect(canChatWithDeviceId('pb_laptop.1-2'), isTrue);
    });
  });

  group('the rule matches the engine store, character for character', () {
    // Mirrors `namespace_dir`: ASCII alphanumeric plus . _ - and nothing else.
    test('every character the store forbids is refused here too', () {
      for (final bad in [
        'has space',
        'colon:here',
        'slash/here',
        r'back\slash',
        'star*',
        'quote"',
        'pipe|',
        'question?',
        'unicodé',
        'new\nline',
      ]) {
        expect(
          canChatWithDeviceId(bad),
          isFalse,
          reason: '$bad would produce a namespace the engine refuses',
        );
      }
    });

    test('the traversal names and the empty id are refused', () {
      expect(canChatWithDeviceId(''), isFalse);
      expect(canChatWithDeviceId('.'), isFalse);
      expect(canChatWithDeviceId('..'), isFalse);
    });

    // A regex anchored with ^...$ still matches across a newline in Dart unless
    // written carefully; an id containing one must not slip through, because
    // the engine would reject the namespace it produces.
    test('an id that is valid only on its first line is refused', () {
      expect(canChatWithDeviceId('good\nbad:id'), isFalse);
    });
  });
}
