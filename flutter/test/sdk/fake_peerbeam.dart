import 'dart:async';

import 'package:peerbeam/sdk/events.dart';
import 'package:peerbeam/sdk/exceptions.dart';
import 'package:peerbeam/sdk/models.dart';
import 'package:peerbeam/sdk/peerbeam.dart';

/// A mock [PeerBeamApi] for repository tests — records calls and lets the test
/// push engine events, with no native library.
class FakePeerBeam implements PeerBeamApi {
  final _ctrl = StreamController<BridgeEvent>.broadcast();
  final List<String> calls = [];
  List<HistoryEntry> historyEntries = [];

  /// When true, [sendFolder] throws instead of succeeding — used to simulate
  /// a mid-batch failure in tests.
  bool failFolder = false;

  void emit(BridgeEvent e) => _ctrl.add(e);

  @override
  bool get available => true;
  @override
  Stream<BridgeEvent> get events => _ctrl.stream;

  @override
  Future<void> initialize({String configJson = ''}) async => calls.add('init');
  @override
  void shutdown() {
    calls.add('shutdown');
    _ctrl.close();
  }

  @override
  Future<void> startDiscovery() async => calls.add('start');
  @override
  Future<void> stopDiscovery() async => calls.add('stop');
  @override
  Future<List<SdkDevice>> devices() async => const [];

  @override
  Future<List<String>> sendFile(PeerTarget peer, List<String> paths) async {
    calls.add('send:${paths.join(",")}');
    return ['tx-1'];
  }

  @override
  Future<String> sendFolder(PeerTarget peer, String path) async {
    if (failFolder) {
      calls.add('sendFolder-fail:$path');
      throw Exception('sendFolder failed');
    }
    calls.add('sendFolder:$path');
    return 'tx-1';
  }

  @override
  Future<void> pause(String id) async => calls.add('pause:$id');
  @override
  Future<void> resume(String id) async => calls.add('resume:$id');
  @override
  Future<void> cancel(String id) async => calls.add('cancel:$id');
  @override
  Future<void> accept(String id) async => calls.add('accept:$id');
  @override
  Future<void> acceptTrust(String id) async => calls.add('acceptTrust:$id');
  @override
  Future<void> reject(String id) async => calls.add('reject:$id');

  @override
  Future<List<TransferSnapshot>> activeTransfers() async => const [];
    Map<String, dynamic> settings = {};

  @override
  Future<void> historyClear() async {
    historyEntries = [];
  }

  @override
  Future<Map<String, dynamic>> settingsGet() async => settings;

  @override
  Future<void> settingsSet(Map<String, dynamic> partial) async {
    settings.addAll(partial);
  }

  List<TrustedDevice> trusted = [];

  @override
  Future<List<TrustedDevice>> trustList() async => trusted;

  @override
  Future<bool> trustRemove(String id) async {
    final before = trusted.length;
    trusted.removeWhere((t) => t.id == id);
    return trusted.length != before;
  }

  @override
  Future<List<HistoryEntry>> history() async {
    calls.add('history');
    return historyEntries;
  }

  final Map<String, List<ChatMessage>> chatHistories = {};
  int _chatSeq = 0;

  /// When true, [chatSendFile] throws instead of persisting a row — the
  /// engine's own behaviour for a path it refuses (missing file, a folder,
  /// an over-long name), where nothing is persisted and nothing is sent.
  bool failChatSendFile = false;

  /// Individual paths [chatSendFile] refuses, for a mixed multi-select where
  /// some files are accepted and some are not.
  final Set<String> refusedFilePaths = {};

  @override
  Future<String> chatSendFile(PeerTarget peer, String path) async {
    calls.add('chatSendFile:$path');
    if (failChatSendFile || refusedFilePaths.contains(path)) {
      throw InvalidArgumentException('cannot read $path');
    }
    // Mirrors `Manager::chat_send_file`: the outgoing row is validated and
    // persisted SYNCHRONOUSLY, before the call returns and before anything is
    // dialed, and it starts life as `transferring`. Keyed by the peer's real
    // id — the same id-not-name guard the text path has.
    final id = 'file-${++_chatSeq}';
    final peerId = peer.id ?? peer.name;
    final name = path.split('/').last;
    chatHistories
        .putIfAbsent(peerId, () => [])
        .add(
          ChatMessage(
            id: id,
            peerId: peerId,
            direction: 'out',
            body: '',
            at: DateTime.now(),
            status: ChatStatusValue.transferring,
            kind: ChatMessageKind.file,
            fileName: name,
            fileSize: 0,
            localPath: path,
          ),
        );
    return id;
  }

  @override
  Future<String> chatSend(PeerTarget peer, String text) async {
    calls.add('chatSend:$text');
    final id = 'chat-${++_chatSeq}';
    // Mirrors the real engine's `device_from`: key by the peer's real id when
    // present, falling back to name only when it isn't (matching the one
    // construction site with no device id — the manual host/port dialog).
    final peerId = peer.id ?? peer.name;
    final msg = ChatMessage(
      id: id,
      peerId: peerId,
      direction: 'out',
      body: text,
      at: DateTime.now(),
      status: 'sent',
    );
    chatHistories.putIfAbsent(peerId, () => []).add(msg);
    return id;
  }

  @override
  Future<List<ChatMessage>> chatHistory(String peerId) async {
    calls.add('chatHistory:$peerId');
    return chatHistories[peerId] ?? const [];
  }
}
