// Which devices can hold a conversation, and which cannot.
//
// A chat row is filed under `chat-<device id>`, and the engine's store rejects
// any namespace outside [A-Za-z0-9._-] (peerbeam-appstore-fs's
// `namespace_dir`). A Tailscale peer's id is `ts:<node id>`; the colon makes
// every store call for it fail with `invalid namespace`. The app used to offer
// the chat button anyway, so the thread opened, stayed empty, and swallowed
// whatever was typed into it.

import 'package:flutter_test/flutter_test.dart';

import 'package:peerbeam/sdk/models.dart' show PeerIdentity, PeerTarget;
import 'package:peerbeam/state/models.dart';

import 'package:peerbeam/data/discovery_repository.dart';

import 'sdk/fake_peerbeam.dart';

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

  group('a provider-scoped peer is resolved by asking the address', () {
    // The engine dials, completes the ordinary handshake, and reports who
    // answered. That is the only way a `ts:` peer or a typed address can get
    // the authenticated id a conversation must be filed under.
    test('identify returns the id that answered, not the one we had', () async {
      final fake = FakePeerBeam();
      fake.identities['100.101.102.103:49600'] = const PeerIdentity(
        deviceId: 'pb-alice-laptop',
        name: 'alice-laptop',
        newlyTrusted: true,
        pairingCode: '482913',
      );
      final repo = DiscoveryRepository(api: fake);
      addTearDown(repo.dispose);

      final found = await repo.identify(
        const PeerTarget(
          id: 'ts:nodeidabc123',
          name: 'alice-laptop',
          addresses: ['100.101.102.103'],
          port: 49600,
        ),
      );

      expect(found, isNotNull);
      expect(found!.deviceId, 'pb-alice-laptop');
      // And the answer is one a conversation can actually be filed under.
      expect(canChatWithDeviceId(found.deviceId), isTrue);
      expect(found.newlyTrusted, isTrue);
      expect(found.pairingCode, '482913');
    });

    test('an unreachable address answers null rather than throwing', () async {
      final fake = FakePeerBeam();
      final repo = DiscoveryRepository(api: fake);
      addTearDown(repo.dispose);

      final found = await repo.identify(
        const PeerTarget(
          id: 'ts:gone',
          name: 'gone',
          addresses: ['100.64.0.9'],
          port: 49600,
        ),
      );

      expect(found, isNull, reason: 'unreachable is an answer, not a crash');
    });

    // A peer picks its own id and nothing on the wire constrains it, so the
    // answer gets the same check the discovered id got.
    test('an answer that still cannot be filed is caught', () {
      expect(canChatWithDeviceId('still:bad'), isFalse);
    });
  });
}
