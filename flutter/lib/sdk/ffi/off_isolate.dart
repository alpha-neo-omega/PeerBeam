// Running a blocking native call on a background isolate.
//
// # Why this exists
//
// Every `PeerBeam` method is declared `Future<...>`, but most of them call the
// native function *synchronously* and hand back an already-completed future.
// For a call that reads a local store that is right: an isolate hop costs more
// than the work. For a call that dials a peer it is a frozen app — the UI
// isolate is the one painting frames, and it cannot paint while it is inside
// `dlopen`ed C code waiting on a network round trip.
//
// The engine bounds three of those (`BROWSE_BUDGET` 10s, `RING_BUDGET` 8s,
// `SYNC_BUDGET` 300s) and a comment in `peerbeam-ffi` says plainly what that
// is worth: *"a synchronous call that blocks the UI for 10s reads as a hang."*
// Bounding a freeze is not fixing it. Two more paths — `pb_chat_mark_read` and
// `pb_chat_react` — have no budget at all and are bounded only by the dial's
// own 30s timeout, and marking-read fires whenever a conversation is opened.
//
// # Why a fresh isolate per call, and not a worker pool
//
// A single long-lived worker would serialise: a 300s `sync_pull` would hold
// the line and a browse behind it would wait 300s for a 10s call. A Dart
// isolate is single-threaded and a blocking native call owns that thread for
// its whole duration, so the only way to avoid head-of-line blocking is more
// isolates. `Isolate.run` gives exactly one per call and reclaims it after.
//
// The spawn costs a millisecond or two against a network round trip measured
// in hundreds, and `DynamicLibrary.open` on an already-loaded image is a
// refcount bump, not a second load. These are user-initiated actions — a tap
// on Browse, opening a conversation — not a hot loop.
//
// # Why this is safe
//
// The engine is a process-global Rust runtime, and Dart isolates are threads
// of one process: the second isolate's `DynamicLibrary.open` reaches the same
// image and the same statics, so it talks to the *same* engine rather than a
// second copy. `peerbeam_ffi::runtime::block_on` runs on a shared multi-thread
// Tokio runtime and takes no global lock, so a call made here genuinely runs
// beside the UI isolate instead of queueing behind it.
//
// Events do not come through here. `pb_set_event_callback` is registered once
// by the UI isolate with a `NativeCallable.listener`, whose whole job is to
// deliver from a foreign thread to its owning isolate's event loop; the engine
// keeps calling that same pointer no matter which isolate asked for the work.
//
// Errors need no marshalling either: the FFI reports failure *in* its JSON
// envelope (`{"ok":false,"error":{...}}`) rather than by throwing, so the raw
// string comes back across the port and `PeerBeam._data` decodes and throws it
// on the UI isolate exactly as it always did.

import 'dart:ffi';
import 'dart:isolate';

import 'package:ffi/ffi.dart';

import 'bindings.dart';

/// How the SDK runs a blocking native call.
///
/// A seam, not indirection for its own sake: no test can load the real engine
/// (CI has no built library, which is why every existing Flutter test runs
/// against `FakePeerBeam`), so the only way to assert that a method actually
/// goes off-isolate is to substitute this.
typedef OffIsolateInvoke = Future<String> Function(String symbol, String? arg);

/// Call `symbol` on a background isolate and return its JSON response.
///
/// `symbol` is a C entry point returning a string the caller frees. Pass `arg`
/// for the one-argument shape most `pb_*` functions have, or `null` for the
/// no-argument shape (`pb_check_updates`) — calling a niladic C function
/// through a one-argument signature is an ABI mismatch, not a spare parameter.
///
/// `libPath` is the same override `Bindings.load` takes, forwarded so the
/// isolate resolves the identical file rather than re-deriving a path that
/// might differ.
Future<String> invokeOffIsolate(String symbol, String? arg, String? libPath) {
  // Only strings cross, which is what keeps this simple: no pointer outlives
  // the isolate that made it, and nothing has to be freed on a thread other
  // than the one that allocated it.
  return Isolate.run(() => _callNative(symbol, arg, libPath));
}

/// The body that runs on the spawned isolate.
String _callNative(String symbol, String? arg, String? libPath) {
  final lib = openPeerbeamLibrary(libPath);
  final free = lib
      .lookupFunction<
        Void Function(Pointer<Utf8>),
        void Function(Pointer<Utf8>)
      >('pb_free_string');

  // The ownership contract is unchanged and still symmetrical, just running
  // somewhere else: Rust allocated the return, so Dart frees it through
  // `pb_free_string`; Dart allocated the argument, so Dart frees that.
  String consume(Pointer<Utf8> out) {
    if (out == nullptr) return '{}';
    try {
      return out.toDartString();
    } finally {
      free(out);
    }
  }

  if (arg == null) {
    final fn = lib
        .lookupFunction<Pointer<Utf8> Function(), Pointer<Utf8> Function()>(
          symbol,
        );
    return consume(fn());
  }

  final fn = lib
      .lookupFunction<
        Pointer<Utf8> Function(Pointer<Utf8>),
        Pointer<Utf8> Function(Pointer<Utf8>)
      >(symbol);
  final argPtr = arg.toNativeUtf8();
  try {
    return consume(fn(argPtr));
  } finally {
    calloc.free(argPtr);
  }
}
