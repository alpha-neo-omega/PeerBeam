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

  /// What the engine would report. Tests override it to prove the About screen
  /// renders whatever the engine says rather than a constant of its own.
  String? engineVersionValue = '9.9.9';

  @override
  String? get engineVersion => engineVersionValue;
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

  /// Transfer ids the engine refuses to accept without a pairing confirmation
  /// — its first-contact gate, as a test can stand it up. An accept for one of
  /// these that does not carry `confirmed: true` fails exactly as the real
  /// engine's does.
  final Set<String> needsPairingConfirmationIds = {};

  void _acceptDecision(String verb, String id, bool confirmed) {
    // Recorded with the answer, so a test can tell an accept that carried the
    // user's confirmation from one that merely happened after a prompt was
    // shown. The two are the whole difference between a check and a formality.
    calls.add(confirmed ? '$verb:$id:confirmed' : '$verb:$id');
    if (needsPairingConfirmationIds.contains(id) && !confirmed) {
      throw InvalidArgumentException(
        'transfer $id is from a device seen for the first time; '
        'confirm the pairing code matches the other device before accepting',
      );
    }
    if (noPendingDecisionIds.contains(id)) {
      throw InvalidArgumentException('no pending transfer $id');
    }
  }

  @override
  Future<void> accept(String id, {bool confirmed = false}) async =>
      _acceptDecision('accept', id, confirmed);
  @override
  Future<void> acceptTrust(String id, {bool confirmed = false}) async =>
      _acceptDecision('acceptTrust', id, confirmed);
  @override
  Future<void> reject(String id) async => _decision('reject', id);

  @override
  Future<List<TransferSnapshot>> activeTransfers() async => const [];

  /// What `pb_transfers_interrupted` would report. Seeded by tests.
  List<InterruptedTransfer> interrupted = [];

  /// Ids `resumeInterrupted` refuses, as the engine does when a checkpoint no
  /// longer binds to its transfer.
  Set<String> unresumableIds = {};

  @override
  Future<List<InterruptedTransfer>> interruptedTransfers() async => interrupted;

  @override
  Future<void> resumeInterrupted(String id, {PeerTarget? peer}) async {
    calls.add('resumeInterrupted:$id');
    if (unresumableIds.contains(id)) {
      throw InvalidArgumentException('cannot resume $id');
    }
    interrupted = interrupted.where((t) => t.id != id).toList();
  }

  @override
  Future<void> discardInterrupted(String id) async {
    calls.add('discardInterrupted:$id');
    interrupted = interrupted.where((t) => t.id != id).toList();
  }

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

  /// The presence snapshot `presence()` returns. Tests set it directly, or
  /// emit `PresenceUpdated` events to drive the repository instead.
  PresenceSnapshot presenceSnapshot = PresenceSnapshot.empty;

  /// When true, [presence] throws instead of answering — used to prove a
  /// transient engine error leaves an accurate dashboard standing rather than
  /// blanking it.
  bool presenceThrows = false;

  /// The last battery reading pushed down, so a test can assert the Android
  /// path actually reached the engine.
  ({int? percent, bool? charging})? pushedBattery;

  @override
  Future<PresenceSnapshot> presence() async {
    if (presenceThrows) throw Exception('presence unavailable');
    return presenceSnapshot;
  }

  @override
  Future<void> presenceBattery({int? percent, bool? charging}) async {
    pushedBattery = (percent: percent, charging: charging);
  }

  /// Every clipboard push the watcher made, in order. **This list is what the
  /// echo-guard test reads**: the property is "a clip received from a peer is
  /// never sent back", and that is only observable as an absence here.
  final List<({String text, int peers})> clipboardPushes = [];

  /// When set, [clipboardSync] throws it — used to prove an over-cap clip is
  /// reported to the user once and not retried every second.
  PeerBeamException? clipboardSyncThrows;

  @override
  Future<int> clipboardSync(String text, List<PeerTarget> peers) async {
    final e = clipboardSyncThrows;
    if (e != null) throw e;
    clipboardPushes.add((text: text, peers: peers.length));
    return peers.length;
  }

  List<TrustedDevice> trusted = [];

  @override
  Future<List<TrustedDevice>> trustList() async => trusted;

  /// Every `trustSetPermission` call, in order — so a test can assert what the
  /// UI actually asked the engine for, not merely what it drew afterwards.
  final List<({String id, String permission, bool granted})> permissionCalls =
      [];

  /// When set, `trustSetPermission` throws it. For the refused-toggle path.
  Object? trustSetPermissionError;

  @override
  Future<bool> trustSetPermission(
    String id,
    String permission,
    bool granted,
  ) async {
    permissionCalls.add((id: id, permission: permission, granted: granted));
    final error = trustSetPermissionError;
    if (error != null) throw error;
    var changed = false;
    trusted = [
      for (final t in trusted)
        if (t.id != id)
          t
        else ...[
          () {
            changed = t.permissions.contains(permission) != granted;
            return TrustedDevice(
              id: t.id,
              name: t.name,
              fingerprint: t.fingerprint,
              trustedAt: t.trustedAt,
              approved: t.approved,
              permissions: {
                ...t.permissions.where((p) => granted || p != permission),
                if (granted) permission,
              },
            );
          }(),
        ],
    ];
    return changed;
  }

  @override
  Future<bool> trustRemove(String id) async {
    final before = trusted.length;
    trusted.removeWhere((t) => t.id == id);
    return trusted.length != before;
  }

  /// The last list [rulesSet] was given, so a test can assert what the UI
  /// actually sent — the **order** above all, since the order is the tie-break.
  List<SaveRule> rulesWritten = [];

  /// When set, [rulesSet] throws it instead of storing. Stands in for the
  /// engine's validation refusing a destination.
  Object? rulesError;

  @override
  Future<int> rulesSet(List<SaveRule> rules) async {
    calls.add('rulesSet:${rules.length}');
    final e = rulesError;
    if (e != null) throw e;
    rulesWritten = List.of(rules);
    return rules.length;
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

  /// The engine's default `limit` (`peerbeam_chat::DEFAULT_SEARCH_LIMIT`),
  /// applied here so a caller that passes none is bounded exactly as the real
  /// engine bounds it — including reporting truncation.
  static const searchDefaultLimit = 50;

  /// What the engine would report for delivery. Tests set it false to prove a
  /// surface distinguishes "applied here" from "the peer saw it".
  bool reactDelivered = true;

  @override
  Future<({bool applied, bool delivered})> chatReact(
    String peerId,
    String messageId,
    String emoji, {
    bool remove = false,
  }) async {
    calls.add('chatReact:$peerId:$messageId:$emoji:$remove');
    final history = chatHistories[peerId];
    final i = history?.indexWhere((m) => m.id == messageId) ?? -1;
    if (history == null || i < 0) {
      return (applied: false, delivered: false);
    }
    final existing = history[i].reactions;
    final at = existing.indexWhere((r) => r.emoji == emoji && r.isMine);
    // Mirrors `ChatStore::apply_reaction`: stating the end state, so applying
    // the same thing twice is applying it once.
    if (remove && at < 0) return (applied: false, delivered: reactDelivered);
    if (!remove && at >= 0) return (applied: false, delivered: reactDelivered);
    final next = [...existing];
    if (remove) {
      next.removeAt(at);
    } else {
      next.add(ChatReaction(emoji: emoji, by: 'out', at: DateTime.now()));
    }
    history[i] = history[i].copyWith(reactions: next);
    return (applied: true, delivered: reactDelivered);
  }

  /// The notes this fake holds, newest edit first once listed. Tombstones are
  /// modelled the way the engine models them — a deleted note stays here so a
  /// test can prove it is neither listed nor editable.
  final List<Note> notes = [];
  final Set<String> deletedNotes = {};
  int _noteSeq = 0;

  @override
  Future<List<Note>> notesList() async {
    calls.add('notesList');
    final live = notes.where((n) => !deletedNotes.contains(n.id)).toList()
      ..sort((a, b) => b.updatedAt.compareTo(a.updatedAt));
    return live;
  }

  @override
  Future<String> notesCreate(String body, {String title = ''}) async {
    calls.add('notesCreate:$title');
    final id = 'note-${++_noteSeq}';
    notes.add(
      Note(id: id, title: title, body: body, updatedAt: DateTime.now()),
    );
    return id;
  }

  @override
  Future<bool> notesEdit(String id, String body, {String title = ''}) async {
    calls.add('notesEdit:$id');
    if (deletedNotes.contains(id)) return false;
    final i = notes.indexWhere((n) => n.id == id);
    if (i < 0) return false;
    notes[i] = Note(
      id: id,
      title: title,
      body: body,
      updatedAt: DateTime.now(),
    );
    return true;
  }

  @override
  Future<bool> notesDelete(String id) async {
    calls.add('notesDelete:$id');
    if (deletedNotes.contains(id)) return false;
    if (!notes.any((n) => n.id == id)) return false;
    deletedNotes.add(id);
    return true;
  }

  /// What the engine would report. False models the default: receipts off.
  bool markReadSent = false;

  @override
  Future<bool> chatMarkRead(String peerId, String readThrough) async {
    calls.add('chatMarkRead:$peerId:$readThrough');
    return markReadSent;
  }

  @override
  Future<ChatSearchResults> chatSearch(String query, {int? limit}) async {
    calls.add('chatSearch:$query');
    final cap = limit ?? searchDefaultLimit;
    // Mirrors `ChatStore::search`: a trimmed, empty query finds nothing rather
    // than everything.
    final needle = query.trim().toLowerCase();
    if (needle.isEmpty) {
      return ChatSearchResults(hits: const [], truncated: false, limit: cap);
    }
    final hits = <ChatSearchHit>[];
    for (final entry in chatHistories.entries) {
      for (final m in entry.value) {
        // Body, then the file's NAME — never its `localPath`, exactly as the
        // engine refuses to search a file's place on disk.
        final haystack = m.isFile ? (m.fileName ?? '') : m.body;
        if (!haystack.toLowerCase().contains(needle)) continue;
        hits.add(
          ChatSearchHit(
            // The conversation the row is filed under, not `m.peerId`.
            peerId: entry.key,
            messageId: m.id,
            at: m.at,
            direction: m.direction,
            kind: m.kind,
            snippet: haystack,
          ),
        );
      }
    }
    // Newest first, ties broken by peer then message id — the engine's own
    // total order.
    hits.sort((a, b) {
      final at = a.at, bt = b.at;
      final byTime = (at == null || bt == null) ? 0 : bt.compareTo(at);
      if (byTime != 0) return byTime;
      final byPeer = a.peerId.compareTo(b.peerId);
      return byPeer != 0 ? byPeer : a.messageId.compareTo(b.messageId);
    });
    return ChatSearchResults(
      hits: hits.take(cap).toList(),
      truncated: hits.length > cap,
      limit: cap,
    );
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
