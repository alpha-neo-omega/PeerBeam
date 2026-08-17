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
  /// Transfer ids for which the engine holds no open decision, so
  /// `accept`/`acceptTrust`/`reject` fail the way the real engine does
  /// (`no pending transfer <id>` → `invalid_argument`). That happens whenever
  /// the prompt timed out, the peer vanished, or the transfer was
  /// auto-accepted and never asked at all.
  final Set<String> noPendingDecisionIds = {};

  void _decision(String verb, String id) {
    calls.add('$verb:$id');
    if (noPendingDecisionIds.contains(id)) {
      throw InvalidArgumentException('no pending transfer $id');
    }
  }

  @override
  Future<void> accept(String id) async => _decision('accept', id);
  @override
  Future<void> acceptTrust(String id) async => _decision('acceptTrust', id);
  @override
  Future<void> reject(String id) async => _decision('reject', id);

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
    // persisted SYNCHRONOUSLY, before the call returns and before a byte is
    // copied or anything is dialed, and it starts life as `staging` (what
    // `begin_file_send` writes) — not `transferring`, which would claim bytes
    // are moving before the copy has begun. Keyed by the peer's real id — the
    // same id-not-name guard the text path has.
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
            status: ChatStatusValue.staging,
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

  /// Transfer ids the engine currently has registered. `chatReconcile` skips
  /// their rows, exactly as `Manager::chat_reconcile` skips anything in
  /// `active` — a row is only orphaned if nothing is still driving it.
  final Set<String> liveTransferIds = {};

  @override
  Future<int> chatReconcile(String peerId) async {
    calls.add('chatReconcile:$peerId');
    // Mirrors `Manager::chat_reconcile`: a row still in flight in the store,
    // with no live transfer behind it, is one nothing will ever finish.
    final rows = chatHistories[peerId];
    if (rows == null) return 0;
    var changed = 0;
    for (var i = 0; i < rows.length; i++) {
      final status = rows[i].status;
      final inFlight =
          status == ChatStatusValue.transferring ||
          status == ChatStatusValue.pendingApproval;
      if (inFlight && !liveTransferIds.contains(rows[i].id)) {
        rows[i] = rows[i].copyWith(status: ChatStatusValue.interrupted);
        changed++;
      }
    }
    return changed;
  }

  /// Message ids the engine still has **queued** for delivery, across all
  /// peers — the fake's stand-in for the outbox. See [_mustKeep].
  final Set<String> queuedMessageIds = {};

  /// When true, [chatDelete] and [chatDeleteMessages] throw instead of
  /// deleting — the engine refuses a delete it cannot make safely (an outbox it
  /// cannot read completely, so the set of records backing queued messages is
  /// unknown), and a surface must not present that as history that went away.
  bool failChatDelete = false;

  /// The one rule both deletes answer to, mirroring the engine's own
  /// `KeepRule`: a row survives a local delete when an outbox entry still names
  /// it, **or** when its bytes are still being staged — no entry names those
  /// yet, and taking the row is what leaves the finished copy queued with
  /// nothing behind it. The real drain reads such a record as "nothing will
  /// ever settle this" and deletes the file's only staged copy.
  ///
  /// Deliberately one predicate here too: the engine's two deletes share one
  /// implementation precisely so they cannot drift, and a fake that let them
  /// drift would happily pass tests the engine could not.
  bool _mustKeep(ChatMessage m) =>
      queuedMessageIds.contains(m.id) || m.status == ChatStatusValue.staging;

  @override
  Future<({int removed, int kept})> chatDelete(String peerId) async {
    calls.add('chatDelete:$peerId');
    if (failChatDelete) throw InternalException('outbox unreadable');
    final rows = chatHistories[peerId];
    if (rows == null) return (removed: 0, kept: 0);
    final keep = rows.where(_mustKeep).toList();
    final removed = rows.length - keep.length;
    // An emptied thread stops existing, exactly as the engine derives its
    // conversation list from the namespaces that still hold records — so the
    // row disappears rather than coming back empty.
    if (keep.isEmpty) {
      chatHistories.remove(peerId);
    } else {
      chatHistories[peerId] = keep;
    }
    return (removed: removed, kept: keep.length);
  }

  @override
  Future<({int removed, List<String> kept})> chatDeleteMessages(
    String peerId,
    List<String> messageIds,
  ) async {
    calls.add('chatDeleteMessages:$peerId/${messageIds.join(",")}');
    if (failChatDelete) throw InternalException('outbox unreadable');
    final rows = chatHistories[peerId];
    if (rows == null) return (removed: 0, kept: const <String>[]);
    final asked = messageIds.toSet();
    // An id the thread does not hold is neither removed nor kept — it is
    // simply not there, which is why this is derived from the ROWS rather than
    // from what was asked for.
    final kept = rows
        .where((m) => asked.contains(m.id) && _mustKeep(m))
        .map((m) => m.id)
        .toList();
    final survivors = rows
        .where((m) => !asked.contains(m.id) || _mustKeep(m))
        .toList();
    final removed = rows.length - survivors.length;
    if (survivors.isEmpty) {
      chatHistories.remove(peerId);
    } else {
      chatHistories[peerId] = survivors;
    }
    return (removed: removed, kept: kept);
  }

  @override
  Future<bool> chatCancel(String peerId, String messageId) async {
    calls.add('chatCancel:$peerId/$messageId');
    final rows = chatHistories[peerId];
    final i = rows?.indexWhere((m) => m.id == messageId) ?? -1;
    if (rows == null || i < 0) return false;
    // Mirrors `ChatRecord::is_cancellable_outgoing_file`: our OWN OUTGOING
    // FILE share, not already settled. Anything else — a text row, the peer's
    // offer to us, a share already delivered or declined — is a clean
    // `false`, not an error, and must not be pretended into a cancel.
    final row = rows[i];
    if (!row.isFile ||
        !row.isMine ||
        row.status == ChatStatusValue.sent ||
        row.status == ChatStatusValue.declined) {
      return false;
    }
    // And, like the engine, settle the row `failed` with the reason and say so
    // on the event stream rather than only in the store — and let go of the
    // queue entry, which is what makes the row removable by a later delete.
    queuedMessageIds.remove(messageId);
    rows[i] = row.copyWith(status: ChatStatusValue.failed);
    emit(
      ChatStatus(
        messageId: messageId,
        peerId: peerId,
        status: ChatStatusValue.failed,
        error: 'cancelled',
      ),
    );
    return true;
  }

  @override
  Future<List<ChatConversation>> chatConversations() async {
    calls.add('chatConversations');
    // Derived from the seeded histories, exactly as `Manager` derives it from
    // the conversation namespaces that exist: one row per thread, newest
    // first, and `unread_hint` counting only INBOUND rows still awaiting a
    // decision (never text, never our own outgoing files).
    final rows = chatHistories.entries.map((e) {
      final msgs = e.value;
      return ChatConversation(
        peerId: e.key,
        lastAt: msgs.isEmpty ? null : msgs.last.at,
        unreadHint: msgs
            .where(
              (m) => !m.isMine && m.status == ChatStatusValue.pendingApproval,
            )
            .length,
      );
    }).toList();
    rows.sort((a, b) {
      final at = a.lastAt, bt = b.lastAt;
      if (at == null && bt == null) return a.peerId.compareTo(b.peerId);
      if (at == null) return 1; // unreadable threads sort last
      if (bt == null) return -1;
      return bt.compareTo(at);
    });
    return rows;
  }
}
