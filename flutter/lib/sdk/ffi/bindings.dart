// Raw `dart:ffi` bindings to `peerbeam-ffi`. This is the ONLY file that touches
// FFI; everything above uses `PeerBeam`. Strings crossing the boundary follow
// the ownership contract: Rust allocates returns (Dart frees via
// `pb_free_string`); Dart allocates args (and frees them).
import 'dart:convert';
import 'dart:ffi';
import 'dart:io';

import 'package:ffi/ffi.dart';

// C signatures.
typedef _AbiC = Uint32 Function();
typedef _AbiDart = int Function();
typedef _RetC = Pointer<Utf8> Function();
typedef _RetDart = Pointer<Utf8> Function();
typedef _ArgRetC = Pointer<Utf8> Function(Pointer<Utf8>);
typedef _ArgRetDart = Pointer<Utf8> Function(Pointer<Utf8>);
typedef _VoidC = Void Function();
typedef _VoidDart = void Function();
typedef _FreeC = Void Function(Pointer<Utf8>);
typedef _FreeDart = void Function(Pointer<Utf8>);
typedef _SetCbC =
    Void Function(Pointer<NativeFunction<Void Function(Pointer<Utf8>)>>);
typedef _SetCbDart =
    void Function(Pointer<NativeFunction<Void Function(Pointer<Utf8>)>>);

/// Thrown when the native library cannot be located/opened.
class NativeLoadError implements Exception {
  final String message;
  NativeLoadError(this.message);
  @override
  String toString() => 'NativeLoadError: $message';
}

/// Bound native functions + JSON marshalling. Construct via [Bindings.load].
class Bindings {
  final _AbiDart _abiVersion;
  final _RetDart _versionJson;
  final _ArgRetDart _init;
  final _VoidDart _shutdown;
  final _SetCbDart _setEventCallback;
  final _FreeDart _freeString;
  final _RetDart _discoveryStart;
  final _RetDart _discoveryStop;
  final _RetDart _devices;
  final _ArgRetDart _send;
  final _ArgRetDart _sendFolder;
  final _ArgRetDart _pause;
  final _ArgRetDart _resume;
  final _ArgRetDart _cancel;
  final _ArgRetDart _accept;
  final _ArgRetDart _reject;
  final _RetDart _active;
  final _ArgRetDart _get;
  final _RetDart _history;
  final _RetDart _trustList;
  final _ArgRetDart _trustRemove;
  final _RetDart _settingsGet;
  final _ArgRetDart _settingsSet;

  Bindings._(DynamicLibrary lib)
    : _abiVersion = lib.lookupFunction<_AbiC, _AbiDart>('pb_abi_version'),
      _versionJson = lib.lookupFunction<_RetC, _RetDart>('pb_version_json'),
      _init = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_init'),
      _shutdown = lib.lookupFunction<_VoidC, _VoidDart>('pb_shutdown'),
      _setEventCallback = lib.lookupFunction<_SetCbC, _SetCbDart>(
        'pb_set_event_callback',
      ),
      _freeString = lib.lookupFunction<_FreeC, _FreeDart>('pb_free_string'),
      _discoveryStart = lib.lookupFunction<_RetC, _RetDart>(
        'pb_discovery_start',
      ),
      _discoveryStop = lib.lookupFunction<_RetC, _RetDart>('pb_discovery_stop'),
      _devices = lib.lookupFunction<_RetC, _RetDart>('pb_devices_json'),
      _send = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_transfer_send'),
      _sendFolder = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_transfer_send_folder',
      ),
      _pause = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_transfer_pause'),
      _resume = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_transfer_resume'),
      _cancel = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_transfer_cancel'),
      _accept = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_transfer_accept'),
      _reject = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_transfer_reject'),
      _active = lib.lookupFunction<_RetC, _RetDart>('pb_transfers_active'),
      _get = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_transfer_get'),
      _history = lib.lookupFunction<_RetC, _RetDart>('pb_history_get'),
      _trustList = lib.lookupFunction<_RetC, _RetDart>('pb_trust_list'),
      _trustRemove = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_trust_remove',
      ),
      _settingsGet = lib.lookupFunction<_RetC, _RetDart>('pb_settings_get'),
      _settingsSet = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_settings_set',
      );

  /// Load the native library. `overridePath` forces a specific file (tests).
  static Bindings load({String? overridePath}) {
    try {
      final lib = _openLibrary(overridePath);
      return Bindings._(lib);
    } on NativeLoadError {
      rethrow;
    } catch (e) {
      throw NativeLoadError('failed to load peerbeam-ffi: $e');
    }
  }

  int abiVersion() => _abiVersion();
  String versionJson() => _consume(_versionJson());
  String init(String configJson) => _withArg(configJson, _init);
  void shutdown() => _shutdown();
  void freeString(Pointer<Utf8> ptr) => _freeString(ptr);
  void setEventCallback(
    Pointer<NativeFunction<Void Function(Pointer<Utf8>)>> cb,
  ) => _setEventCallback(cb);

  String discoveryStart() => _consume(_discoveryStart());
  String discoveryStop() => _consume(_discoveryStop());
  String devices() => _consume(_devices());
  String send(String json) => _withArg(json, _send);
  String sendFolder(String json) => _withArg(json, _sendFolder);
  String pause(String json) => _withArg(json, _pause);
  String resume(String json) => _withArg(json, _resume);
  String cancel(String json) => _withArg(json, _cancel);
  String accept(String json) => _withArg(json, _accept);
  String reject(String json) => _withArg(json, _reject);
  String active() => _consume(_active());
  String get(String json) => _withArg(json, _get);
  String history() => _consume(_history());
  String trustList() => _consume(_trustList());
  String trustRemove(String json) => _withArg(json, _trustRemove);
  String settingsGet() => _consume(_settingsGet());
  String settingsSet(String json) => _withArg(json, _settingsSet);

  /// Read a Rust-owned string and free it (ownership contract).
  String _consume(Pointer<Utf8> ptr) {
    if (ptr == nullptr) return '{}';
    try {
      return ptr.toDartString();
    } finally {
      _freeString(ptr);
    }
  }

  /// Marshal a Dart string argument, call, and free the argument.
  String _withArg(String arg, _ArgRetDart fn) {
    final p = arg.toNativeUtf8();
    try {
      return _consume(fn(p));
    } finally {
      calloc.free(p);
    }
  }
}

/// Open the platform's shared library. iOS links statically (process symbols).
DynamicLibrary _openLibrary(String? overridePath) {
  if (overridePath != null) return DynamicLibrary.open(overridePath);
  if (Platform.isIOS) return DynamicLibrary.process();
  final name = Platform.isWindows
      ? 'peerbeam_ffi.dll'
      : (Platform.isMacOS ? 'libpeerbeam_ffi.dylib' : 'libpeerbeam_ffi.so');
  return DynamicLibrary.open(name);
}

/// Decode a JSON string into a map (utility used by the SDK).
Map<String, dynamic> decodeJson(String s) =>
    jsonDecode(s) as Map<String, dynamic>;
