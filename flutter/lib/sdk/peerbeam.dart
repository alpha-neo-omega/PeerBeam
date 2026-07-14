// The PeerBeam Dart SDK: a clean, typed API over the Rust engine. The app
// (repositories) uses this; it never touches `dart:ffi`. Every engine error
// surfaces as a [PeerBeamException]; a one broadcast [events] stream carries
// all engine events (no polling).
import 'dart:async';
import 'dart:convert';
import 'dart:ffi';

import 'package:ffi/ffi.dart';

import 'events.dart';
import 'exceptions.dart';
import 'ffi/bindings.dart';
import 'models.dart';

/// Expected native ABI. The SDK refuses to run against a mismatched engine.
const int kExpectedAbi = 1;

/// The SDK surface. `PeerBeam` is the real (FFI) implementation; tests use a
/// fake. Methods are async to keep the door open for isolate offloading and to
/// give repositories a uniform await-able API.
abstract class PeerBeamApi {
  /// True when the native engine is loaded and usable.
  bool get available;

  /// All engine events, decoded and typed. Broadcast; never polls.
  Stream<BridgeEvent> get events;

  Future<void> initialize({String configJson = ''});
  void shutdown();

  Future<void> startDiscovery();
  Future<void> stopDiscovery();
  Future<List<SdkDevice>> devices();

  Future<List<String>> sendFile(PeerTarget peer, List<String> paths);
  Future<String> sendFolder(PeerTarget peer, String path);
  Future<void> pause(String id);
  Future<void> resume(String id);
  Future<void> cancel(String id);
  Future<void> accept(String id);
  Future<void> reject(String id);

  Future<List<TransferSnapshot>> activeTransfers();
  Future<List<HistoryEntry>> history();

  /// Persisted engine settings (raw key/value document).
  Future<Map<String, dynamic>> settingsGet();

  /// Merge a partial settings object into the persisted document.
  Future<void> settingsSet(Map<String, dynamic> partial);

  /// Pinned (trusted) devices, newest first.
  Future<List<TrustedDevice>> trustList();

  /// Revoke a pinned device. Returns whether it was pinned.
  Future<bool> trustRemove(String id);
}

/// Real, FFI-backed implementation.
class PeerBeam implements PeerBeamApi {
  Bindings? _b;
  NativeCallable<Void Function(Pointer<Utf8>)>? _callable;
  final StreamController<BridgeEvent> _events =
      StreamController<BridgeEvent>.broadcast();
  bool _initialised = false;

  /// Try to load the native library. Never throws — if the library is absent,
  /// [available] is false and calls throw [PeerBeamUnavailable] so the app
  /// degrades gracefully. `overrideLibPath` targets a specific file (tests).
  PeerBeam({String? overrideLibPath}) {
    try {
      _b = Bindings.load(overridePath: overrideLibPath);
    } on NativeLoadError {
      _b = null;
    }
  }

  @override
  bool get available => _b != null;

  @override
  Stream<BridgeEvent> get events => _events.stream;

  Bindings _req() {
    final b = _b;
    if (b == null) {
      throw const PeerBeamUnavailable('native engine not loaded');
    }
    return b;
  }

  @override
  Future<void> initialize({String configJson = ''}) async {
    final b = _req();
    if (b.abiVersion() != kExpectedAbi) {
      throw InternalException(
        'ABI mismatch: engine ${b.abiVersion()} vs expected $kExpectedAbi',
      );
    }
    // Register the event sink first, so no events are missed after init.
    final callable = NativeCallable<Void Function(Pointer<Utf8>)>.listener(
      _onNativeEvent,
    );
    _callable = callable;
    b.setEventCallback(callable.nativeFunction);
    _data(b.init(configJson));
    _initialised = true;
  }

  /// Native event callback: read + free the Rust string, decode, publish.
  void _onNativeEvent(Pointer<Utf8> ptr) {
    if (ptr == nullptr) return;
    String raw;
    try {
      raw = ptr.toDartString();
    } finally {
      _b?.freeString(ptr);
    }
    try {
      final map = jsonDecode(raw) as Map<String, dynamic>;
      final ev = BridgeEvent.fromJson(map);
      if (ev != null) _events.add(ev);
    } catch (_) {
      // Ignore malformed events rather than crash the isolate.
    }
  }

  @override
  void shutdown() {
    if (_initialised) {
      _b?.shutdown();
      _initialised = false;
    }
    _callable?.close();
    _callable = null;
  }

  @override
  Future<void> startDiscovery() async => _data(_req().discoveryStart());

  @override
  Future<void> stopDiscovery() async => _data(_req().discoveryStop());

  @override
  Future<List<SdkDevice>> devices() async {
    final data = _data(_req().devices());
    return _list(data['devices']).map(SdkDevice.fromJson).toList();
  }

  @override
  Future<List<String>> sendFile(PeerTarget peer, List<String> paths) async {
    final data = _data(
      _req().send(jsonEncode({'peer': peer.toJson(), 'paths': paths})),
    );
    final ids = data['ids'];
    return ids is List ? ids.map((e) => e as String).toList() : const [];
  }

  @override
  Future<String> sendFolder(PeerTarget peer, String path) async {
    final data = _data(
      _req().sendFolder(jsonEncode({'peer': peer.toJson(), 'path': path})),
    );
    return data['id'] as String;
  }

  @override
  Future<void> pause(String id) async => _data(_req().pause(_id(id)));
  @override
  Future<void> resume(String id) async => _data(_req().resume(_id(id)));
  @override
  Future<void> cancel(String id) async => _data(_req().cancel(_id(id)));
  @override
  Future<void> accept(String id) async => _data(_req().accept(_id(id)));
  @override
  Future<void> reject(String id) async => _data(_req().reject(_id(id)));

  @override
  Future<List<TransferSnapshot>> activeTransfers() async {
    final data = _data(_req().active());
    return _list(data['transfers']).map(TransferSnapshot.fromJson).toList();
  }

  @override
  Future<Map<String, dynamic>> settingsGet() async =>
      _data(_req().settingsGet());

  @override
  Future<void> settingsSet(Map<String, dynamic> partial) async =>
      _data(_req().settingsSet(jsonEncode(partial)));

  @override
  Future<List<TrustedDevice>> trustList() async {
    final data = _data(_req().trustList());
    return _list(data['devices']).map(TrustedDevice.fromJson).toList();
  }

  @override
  Future<bool> trustRemove(String id) async {
    final data = _data(_req().trustRemove(jsonEncode({'id': id})));
    return data['removed'] == true;
  }

  @override
  Future<List<HistoryEntry>> history() async {
    final data = _data(_req().history());
    return _list(data['history']).map(HistoryEntry.fromJson).toList();
  }

  // ── envelope handling ─────────────────────────────────────────

  /// Decode a result envelope: return `data`, or throw the typed error.
  Map<String, dynamic> _data(String response) {
    final j = jsonDecode(response) as Map<String, dynamic>;
    if (j['ok'] == true) {
      final d = j['data'];
      return d is Map ? Map<String, dynamic>.from(d) : <String, dynamic>{};
    }
    final e = j['error'] as Map?;
    throw PeerBeamException.fromCode(
      e?['code'] as String? ?? 'internal',
      e?['message'] as String? ?? 'unknown error',
    );
  }

  List<Map<String, dynamic>> _list(dynamic v) => v is List
      ? v.whereType<Map>().map((e) => Map<String, dynamic>.from(e)).toList()
      : const [];

  String _id(String id) => jsonEncode({'id': id});
}
