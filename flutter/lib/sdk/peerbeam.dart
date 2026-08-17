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

  /// Accept AND trust the sending device: future transfers from it are
  /// auto-accepted whenever auto-accept is enabled. A plain [accept] never
  /// does this — trusting a device is always a separate, explicit choice.
  Future<void> acceptTrust(String id);
  Future<void> reject(String id);

  Future<List<TransferSnapshot>> activeTransfers();
  Future<List<HistoryEntry>> history();

  /// Clear all transfer history (persisted).
  Future<void> historyClear();

  /// Persisted engine settings (raw key/value document).
  Future<Map<String, dynamic>> settingsGet();

  /// Merge a partial settings object into the persisted document.
  Future<void> settingsSet(Map<String, dynamic> partial);

  /// Pinned (trusted) devices, newest first.
  Future<List<TrustedDevice>> trustList();

  /// Revoke a pinned device. Returns whether it was pinned.
  Future<bool> trustRemove(String id);

  /// Send a chat text message to a peer. Returns the new message id.
  Future<String> chatSend(PeerTarget peer, String text);

  /// Share the file at [path] inside the conversation with [peer]. Returns the
  /// new message id — which is ALSO the id of the transfer carrying the bytes,
  /// so progress, approval and cancellation all line up with the chat row
  /// without a second correlation table.
  ///
  /// Returns as soon as the row is persisted, which is **before** any byte has
  /// been copied: the row starts life as `staging` while the engine streams the
  /// file into the outbox's own storage, then becomes `pending` (queued) and
  /// finally `sent`. All three report through `chat_status`, and the staging
  /// leg carries a `progress` object.
  ///
  /// **An unreachable peer is queued, not an error** (increment 2b): the bytes
  /// are staged and the entry waits for the peer, exactly like text. Throws
  /// only when the path itself is refused (missing, or a folder), in which case
  /// nothing is persisted and nothing is sent.
  Future<String> chatSendFile(PeerTarget peer, String path);

  /// Call off a file we are sharing: stop the staging copy, stop the transfer,
  /// drop the queue entry, delete the staged bytes, and settle the row.
  ///
  /// Returns whether anything was actually cancelled. **A `false` is honest and
  /// must be believed** — it means the share had already been delivered or
  /// declined (or the id names no row of ours), so a surface that removed the
  /// row optimistically would be reporting a cancel that never happened. It is
  /// not an error, and is safe to call in any state.
  ///
  /// Only ever reaches this device's own outgoing share in [peerId]'s thread;
  /// an inbound offer is refused with the approval prompt, never here.
  Future<bool> chatCancel(String peerId, String messageId);

  /// Every conversation this device holds, newest first.
  ///
  /// Derived from what is on disk rather than from the network, so a peer
  /// discovery cannot currently see still has an openable thread — which is the
  /// whole point of it. See `ChatConversation.unreadHint` for what that field
  /// does and does not mean.
  Future<List<ChatConversation>> chatConversations();

  /// Delete this device's copy of the conversation with [peerId], returning
  /// what actually happened: how many records were `removed`, and how many were
  /// `kept` because they still back a **queued** outbound message.
  ///
  /// **Local only.** Nothing goes on the wire and the peer keeps its own copy —
  /// this is "forget this thread here", never "unsend".
  ///
  /// Anything still waiting to be sent survives, queue entry and staged bytes
  /// included, and will still be sent. That is not a nicety: the engine's drain
  /// reads a *missing* conversation record as "nothing will ever settle this"
  /// and throws the queued file away, so a delete that took those rows with it
  /// would destroy the file minutes later from a background tick. The thread
  /// therefore stays listed exactly when something is still going out — visible
  /// straight away rather than reappearing out of nowhere later.
  ///
  /// Both counts are the engine's own, so a surface can report the outcome
  /// rather than guess at it.
  Future<({int removed, int kept})> chatDelete(String peerId);

  /// Delete [messageIds] from the conversation with [peerId], returning what
  /// actually happened: how many rows were `removed`, and the ids that were
  /// `kept` because a **queued** outbound send still depends on them.
  ///
  /// **Local only**, exactly like [chatDelete]: nothing goes on the wire and the
  /// peer keeps its own copy — this is "forget these messages here", never
  /// "unsend".
  ///
  /// `kept` is a list of ids rather than a count because the user pointed at
  /// particular messages: a surface can name the ones it could not take and say
  /// why. They are kept for the same reason a conversation delete keeps them —
  /// the engine's drain reads a *missing* record as "nothing will ever settle
  /// this" and throws the queued file away — and both deletes share one
  /// implementation of that rule, so they cannot drift apart.
  ///
  /// An id the conversation does not hold is neither removed nor kept; it is
  /// simply not there. An empty [messageIds] deletes nothing and reports
  /// nothing, rather than failing.
  Future<({int removed, List<String> kept})> chatDeleteMessages(
    String peerId,
    List<String> messageIds,
  );

  /// Chat history with a given peer, oldest first. A pure read.
  Future<List<ChatMessage>> chatHistory(String peerId);

  /// Settle rows in this conversation that no event will ever finish — a file
  /// left mid-flight by a crash or a hard restart — and return how many were
  /// changed. Rows whose transfer is live right now are left alone.
  ///
  /// Call it when a thread is opened, **before** rendering its history: the
  /// engine's startup pass can only reach peers with queued text, so a
  /// file-only thread would otherwise show an eternal progress bar or offer an
  /// Accept button for a transfer that no longer exists.
  Future<int> chatReconcile(String peerId);
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
  Future<void> acceptTrust(String id) async =>
      _data(_req().acceptTrust(_id(id)));
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

  @override
  Future<void> historyClear() async => _data(_req().historyClear());

  @override
  Future<String> chatSend(PeerTarget peer, String text) async {
    final data = _data(
      _req().chatSend(jsonEncode({'peer': peer.toJson(), 'text': text})),
    );
    return data['id'] as String;
  }

  @override
  Future<String> chatSendFile(PeerTarget peer, String path) async {
    final data = _data(
      _req().chatSendFile(jsonEncode({'peer': peer.toJson(), 'path': path})),
    );
    return data['id'] as String;
  }

  @override
  Future<List<ChatMessage>> chatHistory(String peerId) async {
    final data = _data(
      _req().chatHistory(jsonEncode({'peer_id': peerId})),
    );
    return _list(data['messages']).map(ChatMessage.fromJson).toList();
  }

  @override
  Future<int> chatReconcile(String peerId) async {
    final data = _data(
      _req().chatReconcile(jsonEncode({'peer_id': peerId})),
    );
    return (data['changed'] as num?)?.toInt() ?? 0;
  }

  @override
  Future<bool> chatCancel(String peerId, String messageId) async {
    final data = _data(
      _req().chatCancel(
        jsonEncode({'peer_id': peerId, 'message_id': messageId}),
      ),
    );
    return data['cancelled'] == true;
  }

  @override
  Future<List<ChatConversation>> chatConversations() async {
    // The export takes no arguments; `{}` is the empty request it expects.
    final data = _data(_req().chatConversations('{}'));
    return _list(data['peers']).map(ChatConversation.fromJson).toList();
  }

  @override
  Future<({int removed, int kept})> chatDelete(String peerId) async {
    final data = _data(_req().chatDelete(jsonEncode({'peer_id': peerId})));
    return (
      removed: (data['removed'] as num?)?.toInt() ?? 0,
      kept: (data['kept'] as num?)?.toInt() ?? 0,
    );
  }

  @override
  Future<({int removed, List<String> kept})> chatDeleteMessages(
    String peerId,
    List<String> messageIds,
  ) async {
    final data = _data(
      _req().chatDeleteMessages(
        jsonEncode({'peer_id': peerId, 'message_ids': messageIds}),
      ),
    );
    final kept = data['kept'];
    return (
      removed: (data['removed'] as num?)?.toInt() ?? 0,
      kept: kept is List ? kept.whereType<String>().toList() : <String>[],
    );
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
