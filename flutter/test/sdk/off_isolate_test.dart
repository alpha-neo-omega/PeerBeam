// The SDK calls that dial a peer must not run on the UI isolate.
//
// These tests substitute the isolate hop (`invokeOff`) rather than performing
// it, so they run everywhere — including CI, which has no built engine. What
// they can prove is that each of these methods goes through the off-isolate
// seam at all, reaches the right C entry point with the right payload, and
// still raises the same typed exceptions on the caller's side.
//
// What they cannot prove is that the UI isolate actually stays responsive
// while the native call runs. That needs the real library and lives in
// `ffi_test.dart`, which skips when the cdylib is not built.

import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/sdk/exceptions.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/sdk/peerbeam.dart';

/// Records what the SDK asked for and answers with a canned envelope.
class _Recorder {
  final List<({String symbol, String? arg})> calls = [];
  String response = '{"ok":true,"data":{}}';

  Future<String> call(String symbol, String? arg) async {
    calls.add((symbol: symbol, arg: arg));
    return response;
  }
}

void main() {
  late _Recorder rec;
  late PeerBeam api;

  setUp(() {
    rec = _Recorder();
    // No `overrideLibPath`, so the real library is absent and `_b` is null —
    // which is exactly the state CI runs in, and the reason `_off` has to
    // check availability itself.
    api = PeerBeam(invokeOff: rec.call);
  });

  const peer = PeerTarget(
    name: 'laptop',
    addresses: ['10.0.0.2'],
    port: 49600,
    id: 'pb-1234',
  );

  group('a call that dials a peer goes off-isolate', () {
    test('browse asks pb_browse_list with the peer and path', () async {
      rec.response = jsonEncode({
        'ok': true,
        'data': {'path': 'docs', 'entries': <dynamic>[]},
      });
      await api.browse(peer, path: 'docs');

      expect(rec.calls.single.symbol, 'pb_browse_list');
      final sent = jsonDecode(rec.calls.single.arg!) as Map<String, dynamic>;
      expect(sent['path'], 'docs');
      expect((sent['peer'] as Map)['id'], 'pb-1234');
    });

    test('syncFolder asks pb_sync_pull', () async {
      rec.response = jsonEncode({
        'ok': true,
        'data': {'wanted': 0, 'pushed': 0, 'conflicts': <dynamic>[]},
      });
      await api.syncFolder(peer, 'docs', '/tmp/into');

      expect(rec.calls.single.symbol, 'pb_sync_pull');
      final sent = jsonDecode(rec.calls.single.arg!) as Map<String, dynamic>;
      expect(sent['into'], '/tmp/into');
    });

    test('presenceRing asks pb_presence_ring', () async {
      rec.response = jsonEncode({
        'ok': true,
        'data': {'sent': true},
      });
      expect(await api.presenceRing(peer, seconds: 3), isTrue);
      expect(rec.calls.single.symbol, 'pb_presence_ring');
    });

    test('notesSync asks pb_notes_sync', () async {
      rec.response = jsonEncode({
        'ok': true,
        'data': {'sent': true},
      });
      expect(await api.notesSync(peer), isTrue);
      expect(rec.calls.single.symbol, 'pb_notes_sync');
    });

    /// The one with no budget on the engine side, and the one that fires
    /// whenever a conversation is opened — so the freeze it caused was the
    /// most-hit of the set.
    test('chatMarkRead asks pb_chat_mark_read', () async {
      rec.response = jsonEncode({
        'ok': true,
        'data': {'sent': true},
      });
      expect(await api.chatMarkRead('pb-1234', 'msg-9'), isTrue);

      expect(rec.calls.single.symbol, 'pb_chat_mark_read');
      final sent = jsonDecode(rec.calls.single.arg!) as Map<String, dynamic>;
      expect(sent['read_through'], 'msg-9');
    });

    test('chatReact asks pb_chat_react', () async {
      rec.response = jsonEncode({
        'ok': true,
        'data': {'applied': true, 'delivered': false},
      });
      final r = await api.chatReact('pb-1234', 'msg-9', '👍');
      expect(r.applied, isTrue);
      expect(r.delivered, isFalse);
      expect(rec.calls.single.symbol, 'pb_chat_react');
    });

    /// Takes no argument, so it must be invoked through the niladic C
    /// signature. Passing a pointer to a function that declares no parameter
    /// is an ABI mismatch, not a spare argument.
    test('checkForUpdates asks pb_check_updates with no argument', () async {
      rec.response = jsonEncode({
        'ok': true,
        'data': {'current': '0.10.0', 'latest': '0.10.0', 'update': false},
      });
      await api.checkForUpdates();

      expect(rec.calls.single.symbol, 'pb_check_updates');
      expect(rec.calls.single.arg, isNull);
    });
  });

  group('nothing about error handling changed', () {
    /// The envelope is decoded on the *caller's* isolate, so a refusal is the
    /// same typed exception it always was. If the failure were marshalled
    /// across the port instead, this is the test that would catch the
    /// difference.
    test('a refusal arrives as its typed exception', () async {
      rec.response = jsonEncode({
        'ok': false,
        'error': {'code': 'permission_denied', 'message': 'browse not granted'},
      });
      await expectLater(
        api.browse(peer),
        throwsA(
          isA<PeerBeamException>().having(
            (e) => e.message,
            'message',
            contains('browse not granted'),
          ),
        ),
      );
    });

    /// Availability is still decided here, before the hop. Otherwise an absent
    /// engine would surface as a library-load failure inside a spawned isolate
    /// — a place the caller cannot catch it from.
    test('an absent engine still throws PeerBeamUnavailable', () async {
      final noEngine = PeerBeam();
      expect(noEngine.available, isFalse);
      await expectLater(
        noEngine.browse(peer),
        throwsA(isA<PeerBeamUnavailable>()),
      );
    });
  });
}
