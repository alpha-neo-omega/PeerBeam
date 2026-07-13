// Real FFI integration: load the built cdylib and drive the SDK the way the app
// does — init, discovery, error mapping (typed exceptions over real FFI), event
// stream delivery, and a stress/leak loop. Skipped if the library isn't built.

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/exceptions.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/sdk/peerbeam.dart';

String? _libPath() {
  final base = Directory.current.path; // the flutter/ dir under `flutter test`
  for (final rel in [
    '../rust/target/debug/libpeerbeam_ffi.so',
    '../rust/target/release/libpeerbeam_ffi.so',
  ]) {
    final f = File('$base/$rel');
    if (f.existsSync()) return f.absolute.path;
  }
  return null;
}

void main() {
  final lib = _libPath();
  final skip = lib == null ? 'cdylib not built (run: cargo build -p peerbeam-ffi)' : false;

  group('real FFI', () {
    late PeerBeam api;

    setUp(() {
      api = PeerBeam(overrideLibPath: lib);
    });
    tearDown(() => api.shutdown());

    test('loads, initialises, and lists devices', () async {
      expect(api.available, isTrue);
      await api.initialize();
      final devices = await api.devices();
      expect(devices, isA<List<SdkDevice>>()); // empty before discovery
      await api.startDiscovery();
      await api.stopDiscovery();
    });

    test('maps engine errors to typed Dart exceptions', () async {
      await api.initialize();
      // Unknown transfer id → invalid_argument → InvalidArgumentException.
      await expectLater(
        () => api.pause('does-not-exist'),
        throwsA(isA<InvalidArgumentException>()),
      );
    });

    test('delivers engine events over the FFI callback', () async {
      await api.initialize();
      // `transfer_queued` is emitted synchronously when a send is registered —
      // proves the Rust → NativeCallable → Dart event path without depending on
      // network failure timing.
      final queued = api.events.firstWhere(
        (e) => e is TransferEvent && e.kind == 'transfer_queued',
      );
      final ids = await api.sendFile(
        const PeerTarget(name: 'nobody', addresses: ['127.0.0.1'], port: 1),
        [Platform.resolvedExecutable], // any existing file path
      );
      expect(ids, isNotEmpty);
      final ev = await queued.timeout(const Duration(seconds: 5));
      expect((ev as TransferEvent).transferId, isNotEmpty);
      // Clean up the (parked) transfer.
      await api.cancel(ids.first);
    });

    test('stress: many calls stay bounded and stable', () async {
      await api.initialize();
      for (var i = 0; i < 500; i++) {
        await api.devices();
        await api.activeTransfers();
      }
      // No crash / hang → the string-ownership contract holds under load.
    });
  }, skip: skip);
}
