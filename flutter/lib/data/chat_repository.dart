// ignore_for_file: prefer_initializing_formals
import 'dart:async';

import 'package:flutter/foundation.dart';

import '../sdk/events.dart';
import '../sdk/exceptions.dart';
import '../sdk/models.dart';
import '../sdk/peerbeam.dart';

/// Per-peer chat conversations, refetched from the engine on demand and
/// appended-to live as `chat_received` events arrive.
///
/// Conversations are keyed by the peer's real device id, passed explicitly
/// as `peerId` alongside the [PeerTarget] used to actually send. [PeerTarget]
/// does carry its own `id` field now, but it's optional — a manually-entered
/// host:port target has none — so callers (the chat screen) still thread the
/// device id through separately rather than reading it off [PeerTarget].
class ChatRepository extends ChangeNotifier {
  final PeerBeamApi? _api;
  final Map<String, List<ChatMessage>> _byPeer = {};

  /// Why a row failed, keyed by message id. Session-scoped on purpose: the
  /// engine persists the failed *status* but not a reason, and the reason
  /// arrives only on the `chat_status` event that reports it.
  final Map<String, String> _errors = {};

  /// Rows the engine deliberately never persisted, per peer.
  ///
  /// `chat_send_file` validates the path *before* it writes anything, so a
  /// refused one (a folder, a file that moved, an over-long name) leaves no
  /// record at all — and [refresh] rebuilds a conversation from engine history
  /// alone. Without this, the "it never left" bubble would be erased by the
  /// very next reconcile: a sibling in the same multi-select, the next text
  /// message, an incoming file settling, or simply reopening the thread. They
  /// are re-appended by [refresh] and cleared only by [dismiss].
  final Map<String, List<ChatMessage>> _unsent = {};

  /// How far each staging copy has got, keyed by message id.
  ///
  /// Session-scoped like [_errors], and for the same reason: the engine
  /// persists the row's *status*, not its progress, which exists only on the
  /// `chat_status` events that report it. An entry is retired the moment the
  /// row leaves `staging` — the bar has nothing left to say once the copy is
  /// queued, cancelled or failed.
  final Map<String, ({int done, int total})> _staging = {};

  /// Every conversation on disk, newest first (see [refreshConversations]).
  List<ChatConversation> _conversations = const [];
  bool _loadingConversations = false;
  bool _conversationsStale = false;

  /// Why the last read of a conversation failed, keyed by peer id.
  ///
  /// A read that did not come back and a thread with nothing in it are
  /// different facts that [_byPeer] alone cannot tell apart: a failed read
  /// leaves exactly the missing entry an empty conversation does, so a surface
  /// reading only that tells the user their history is gone when the truth is
  /// that nobody answered. Kept per peer because every conversation lives in
  /// here at once — a thread that failed must not make the one beside it look
  /// broken.
  final Map<String, Object> _loadErrors = {};

  /// Why the last read of the conversation list failed. Same distinction as
  /// [_loadErrors], for the list itself.
  Object? _conversationsError;

  StreamSubscription<BridgeEvent>? _sub;
  bool _disposed = false;
  int _optimisticSeq = 0;

  /// Note: does NOT refresh in the constructor — same reasoning as
  /// `TrustRepository`/`HistoryRepository`. Repositories are constructed
  /// synchronously in `AppState.live` during `initState`, before the engine's
  /// `initialize()` has been awaited; an early `refresh()` would just hit
  /// `not_initialised` and be swallowed. Chat is additionally per-peer, so
  /// there is no single boot-time refresh anyway — the chat screen refreshes
  /// its own conversation when it opens.
  ChatRepository({PeerBeamApi? api}) : _api = api {
    _sub = _api?.events.listen((e) {
      if (e is ChatReceived) _onReceived(e.message);
      if (e is ChatStatus) _onStatus(e);
    });
  }

  /// Messages for one conversation, oldest first (as returned by the engine).
  List<ChatMessage> messagesFor(String peerId) =>
      List.unmodifiable(_byPeer[peerId] ?? const []);

  /// Every conversation on disk, newest first. Empty until
  /// [refreshConversations] has run.
  List<ChatConversation> get conversations => List.unmodifiable(_conversations);

  /// How far the staging copy behind [messageId] has got, or null when this
  /// session has seen no progress for it — which is the ordinary state for the
  /// first moment of a share, and permanently so for one a restart interrupted.
  /// A surface must render that as an indeterminate bar, never as 0%.
  ({int done, int total})? stagingFor(String messageId) => _staging[messageId];

  /// Why the last read of the conversation with [peerId] failed, or null when
  /// it came back — including the ordinary case of a thread never read at all.
  /// See [_loadErrors] for why an absence and a failure are kept apart.
  Object? loadErrorFor(String peerId) => _loadErrors[peerId];

  /// Why the last read of the conversation list failed, or null when it came
  /// back. "No conversations yet" is a claim about this device's own disk, and
  /// a read that failed has no standing to make it.
  Object? get conversationsError => _conversationsError;

  /// Why the message with [messageId] failed, when the engine said so. Null
  /// for every message that hasn't failed (and for a failure this session
  /// never saw — the reason isn't persisted).
  String? errorFor(String messageId) => _errors[messageId];

  /// Whether this row exists only in this session because the engine refused
  /// to send it — nothing was persisted, so the user is the only one who can
  /// clear it (see [dismiss]).
  bool isUnsent(String peerId, String messageId) =>
      _unsent[peerId]?.any((m) => m.id == messageId) ?? false;

  /// Acknowledge an unsent row: drop it from the conversation for good.
  void dismiss(String peerId, String messageId) {
    _unsent[peerId]?.removeWhere((m) => m.id == messageId);
    if (_unsent[peerId]?.isEmpty ?? false) _unsent.remove(peerId);
    _errors.remove(messageId);
    _byPeer[peerId]?.removeWhere((m) => m.id == messageId);
    notifyListeners();
  }

  /// Open a conversation: settle anything a crash left mid-flight, then load
  /// it.
  ///
  /// The reconcile runs **before** the read, and its result is rendered rather
  /// than the pre-reconcile state: a file row that survived a restart as
  /// `transferring`/`pendingapproval` is one nothing will ever complete, so
  /// showing it as in-flight means an eternal progress bar and — worse — an
  /// Accept button for a transfer that no longer exists. A reconcile failure
  /// is not fatal: the conversation still loads.
  Future<void> openThread(String peerId) async {
    try {
      await _api?.chatReconcile(peerId);
    } catch (_) {
      // Best-effort: an unreconciled row is stale, not wrong to show.
    }
    if (_disposed) return;
    await refresh(peerId);
  }

  /// Pull the persisted conversation with [peerId] from the engine.
  ///
  /// A failure keeps whatever is already on screen — a stale conversation is
  /// still the user's conversation — but is remembered rather than swallowed
  /// (see [loadErrorFor]): a thread that has never loaded is otherwise
  /// indistinguishable from one with nothing in it, and only one of those two
  /// is something the surface may state as fact.
  Future<void> refresh(String peerId) async {
    final api = _api;
    if (api == null) return;
    try {
      final msgs = await api.chatHistory(peerId);
      if (_disposed) return;
      // Copied, not stored by reference: this repository appends to its own
      // lists (optimistic rows, live `chat_received`), so holding the SDK's
      // list would both mutate the caller's data and break outright on a
      // fixed-length/const one.
      //
      // Engine history is authoritative for everything it knows about, but it
      // cannot know about a share the engine refused before persisting
      // anything — so those rows are carried across, newest last (they were
      // created just now, and nothing will ever deliver them). See [_unsent].
      _byPeer[peerId] = [...msgs, ...?_unsent[peerId]];
      _loadErrors.remove(peerId);
      notifyListeners();
    } catch (e) {
      // Keep the current view on transient errors — but say so, rather than
      // leaving the surface to present the failure as an empty thread.
      if (_disposed) return;
      _loadErrors[peerId] = e;
      notifyListeners();
    }
  }

  /// Pull the list of conversations from the engine.
  ///
  /// This is what makes a thread reachable for a peer discovery cannot see —
  /// it is derived from what is on disk, not from the network. Overlapping
  /// calls are coalesced rather than queued: the engine reads every record of
  /// every conversation to build this, and a burst of arrivals (an outbox
  /// draining fifty queued messages at once) must not turn into fifty full
  /// scans. A request that lands mid-flight sets [_conversationsStale] and is
  /// served by one extra pass at the end.
  Future<void> refreshConversations() async {
    final api = _api;
    if (api == null) return;
    if (_loadingConversations) {
      _conversationsStale = true;
      return;
    }
    _loadingConversations = true;
    try {
      do {
        _conversationsStale = false;
        final list = await api.chatConversations();
        if (_disposed) return;
        _conversations = list;
        _conversationsError = null;
        notifyListeners();
      } while (_conversationsStale);
    } catch (e) {
      // Keep the current list on transient errors — and remember why, because
      // an unread list renders exactly like a device that has never chatted,
      // and the thread it would hide may be the only way back to a queued file.
      if (_disposed) return;
      _conversationsError = e;
      notifyListeners();
    } finally {
      _loadingConversations = false;
    }
  }

  /// Delete this device's copy of one conversation, returning the engine's own
  /// counts: how many records were `removed`, and how many were `kept` because
  /// they still back a queued outbound message.
  ///
  /// **Local only** — nothing goes on the wire, and the peer keeps its copy.
  ///
  /// The cached thread is dropped and the conversation list refreshed, in that
  /// order, so the surface that asked never renders rows the engine has just
  /// removed. What the engine *kept* comes straight back in the refresh: a
  /// thread with something still going out stays listed, which is the honest
  /// outcome and the one the confirmation promised.
  ///
  /// Session-only state for the thread goes too ([_errors], [_staging],
  /// [_unsent]): most of it describes rows that no longer exist, and an unsent
  /// row in particular — one the engine never persisted — would otherwise be
  /// re-appended by the very next [refresh], resurrecting a message the user
  /// just deleted. A *kept* row loses only its live staging progress, which its
  /// next `chat_status` event restores; until then its bar is indeterminate,
  /// which is what that row already renders before its first progress event.
  Future<({int removed, int kept})> deleteConversation(String peerId) async {
    final api = _api;
    if (api == null) return (removed: 0, kept: 0);
    final result = await api.chatDelete(peerId);
    if (_disposed) return result;
    for (final m in _byPeer[peerId] ?? const <ChatMessage>[]) {
      _errors.remove(m.id);
      _staging.remove(m.id);
    }
    _byPeer.remove(peerId);
    _unsent.remove(peerId);
    // The thread is gone, so a failure to read it is no longer news about
    // anything — left behind it would raise an error page over a conversation
    // the user has just deleted.
    _loadErrors.remove(peerId);
    notifyListeners();
    await refreshConversations();
    return result;
  }

  /// Delete some of a conversation's messages, returning the engine's own
  /// answer: how many rows were `removed`, and the ids it `kept` because they
  /// still back a queued outbound message.
  ///
  /// **Local only** — nothing goes on the wire, and the peer keeps its copy.
  ///
  /// The cache is narrowed to what the engine actually did, never to what was
  /// asked: a kept row stays exactly where it was, so a file still being sent
  /// keeps its bubble, its progress bar and its Cancel. Everything else the
  /// caller named is dropped, along with the session-only state describing it
  /// ([_errors], [_staging], [_unsent]) — an unsent row in particular, which the
  /// engine never persisted and [refresh] would otherwise re-append,
  /// resurrecting a message the user just deleted.
  ///
  /// An id the engine reports as neither removed nor kept is one it never
  /// held — an unsent row is exactly that — and dropping it here is the only
  /// way it can ever go, which is what [dismiss] already does for the same
  /// rows.
  ///
  /// The conversation list is re-read afterwards because a thread whose last
  /// row has just gone stops being a thread.
  Future<({int removed, List<String> kept})> deleteMessages(
    String peerId,
    List<String> messageIds,
  ) async {
    final api = _api;
    if (api == null) return (removed: 0, kept: const <String>[]);
    final result = await api.chatDeleteMessages(peerId, messageIds);
    if (_disposed) return result;
    final kept = result.kept.toSet();
    final gone = messageIds.where((id) => !kept.contains(id)).toSet();
    for (final id in gone) {
      _errors.remove(id);
      _staging.remove(id);
    }
    _byPeer[peerId]?.removeWhere((m) => gone.contains(m.id));
    _unsent[peerId]?.removeWhere((m) => gone.contains(m.id));
    if (_unsent[peerId]?.isEmpty ?? false) _unsent.remove(peerId);
    notifyListeners();
    await refreshConversations();
    return result;
  }

  /// Search this device's stored conversations, newest match first.
  ///
  /// A **passthrough**, deliberately: results belong to the query that asked
  /// for them, so they are handed straight back to the caller rather than
  /// cached here and re-shown for a query that has since changed. Nothing is
  /// notified — no repository state moves — so a search cannot rebuild the
  /// conversation list or any open thread.
  ///
  /// The engine does the searching (see [PeerBeamApi.chatSearch]). A filter
  /// here would mean pulling every message of every conversation across the FFI
  /// to answer one query.
  ///
  /// A failure **throws**, and is deliberately not flattened to
  /// [ChatSearchResults.empty]. An empty result set is the engine stating that
  /// the message is not there; a search that never ran has stated nothing at
  /// all. Returning the first for the second tells someone searching their own
  /// history — authoritatively — that what they are looking for does not
  /// exist, and that is the one answer a search cannot take back: they stop
  /// looking. There is no repository state to protect here (nothing is cached,
  /// nothing is notified), so the caller that owns the query owns the failure
  /// too, and says which of the two it has.
  Future<ChatSearchResults> search(String query, {int? limit}) async {
    final api = _api;
    // Not a failure: a build with no engine wired has nothing to search, the
    // same null guard every other method here takes.
    if (api == null) return ChatSearchResults.empty;
    return api.chatSearch(query, limit: limit);
  }

  /// Tell the peer we have read its messages, up to the newest one we hold.
  ///
  /// Sends nothing unless the user opted in — the engine decides that, not this
  /// class — so calling it on every thread open is safe and is *not* a
  /// disclosure by itself. Nothing is stored locally: whether we have read a
  /// peer's messages is the surface's business, and persisting it here would
  /// invent a second read-state no wire message maintains.
  Future<void> markRead(String peerId) async {
    final api = _api;
    if (api == null) return;
    // The watermark is the newest message *they* sent: telling a peer we read
    // our own messages would be nonsense.
    final theirs = _byPeer[peerId]?.where((m) => !m.isMine).toList();
    if (theirs == null || theirs.isEmpty) return;
    try {
      await api.chatMarkRead(peerId, theirs.last.id);
    } catch (_) {
      // A receipt is a courtesy; failing to send one is never worth surfacing.
    }
  }

  /// React to a message, or withdraw that reaction.
  ///
  /// Returns whether the peer was told. The local half is applied by the
  /// engine either way and re-read here, so the reaction appears on this
  /// device even when the peer is unreachable — but the caller is handed the
  /// delivery answer so it can say so rather than implying the gesture landed.
  Future<bool> react(
    String peerId,
    String messageId,
    String emoji, {
    bool remove = false,
  }) async {
    final api = _api;
    if (api == null) return false;
    try {
      final r = await api.chatReact(peerId, messageId, emoji, remove: remove);
      if (r.applied) await refresh(peerId);
      return r.delivered;
    } catch (_) {
      return false;
    }
  }

  /// Call off a file we are sharing, and report **honestly** whether anything
  /// was cancelled.
  ///
  /// A `false` is not a failure to handle away: the engine is telling us the
  /// share had already been delivered or declined, so the row must NOT be
  /// removed or relabelled as though the user had stopped it. The conversation
  /// is re-read instead, which replaces whatever the user was looking at with
  /// what the row actually is; the caller is expected to say so as well.
  ///
  /// Nothing is re-read on success: the engine settles the row and emits the
  /// `chat_status` that [_onStatus] applies, so refreshing here would be a
  /// second, racier path to the same state.
  Future<bool> cancelFile(String peerId, String messageId) async {
    final api = _api;
    if (api == null) return false;
    var cancelled = false;
    try {
      cancelled = await api.chatCancel(peerId, messageId);
    } catch (_) {
      // A refused id (never persisted, so never cancellable) reads exactly
      // like the engine's own "there was nothing to cancel".
      cancelled = false;
    }
    if (_disposed) return cancelled;
    if (!cancelled) await refresh(peerId);
    return cancelled;
  }

  /// Send [text] to [peer], filed under [peerId] in the conversation map.
  ///
  /// `chatSend` enqueues the message durably and returns immediately (1b:
  /// offline-first send) — it does not block on a dial/handshake. Delivery
  /// happens via an opportunistic flush right after enqueueing, plus a
  /// background drain/flush-on-connect that keeps retrying indefinitely
  /// while the peer stays unreachable. An optimistic outgoing message is
  /// appended immediately — before the await — so the UI feels instant;
  /// `refresh` reconciles with the persisted record once the call resolves,
  /// and a `chat_status` event later flips the message's status in place
  /// (via [_onStatus]) once it's actually delivered.
  Future<void> send(String peerId, PeerTarget peer, String text) async {
    final body = text.trim();
    if (body.isEmpty) return;
    final optimistic = ChatMessage(
      id: 'local-${++_optimisticSeq}',
      peerId: peerId,
      direction: 'out',
      body: body,
      at: DateTime.now(),
      status: 'pending',
    );
    (_byPeer[peerId] ??= <ChatMessage>[]).add(optimistic);
    notifyListeners();
    try {
      await _api?.chatSend(peer, body);
      await refresh(peerId);
      // The first message to a peer CREATES the conversation, and nothing else
      // will announce it: an offline peer sends back no record and settles no
      // status, so without this the thread would be missing from the
      // Conversations list for exactly as long as it is unreachable — which is
      // when reaching it matters most.
      unawaited(refreshConversations());
    } catch (_) {
      // chatSend itself only fails on a local/validation error (enqueueing
      // is durable and always attempted) — an unreachable peer is not an
      // error here, it stays queued in the engine's outbox and is retried by
      // the drain/flush-on-connect until delivered. Leave the optimistic
      // message in place; `_onStatus` will update it once delivery happens.
    }
  }

  /// Share the file at [path] inside the conversation with [peer].
  ///
  /// Mirrors [send], including the outbox: since increment 2b an unreachable
  /// peer no longer fails the share — the bytes are staged into the engine's
  /// own storage and the entry waits, exactly like a queued text message. What
  /// is still worth showing (and is not swallowed) is a refusal of the *path*,
  /// which happens before anything is persisted.
  ///
  /// [name] and [size] are only used for the optimistic row shown before the
  /// engine answers; the persisted record is authoritative from `refresh` on.
  /// The engine validates and persists the row synchronously (before it copies
  /// or dials anything), so that reconcile always finds it.
  ///
  /// Call this once per picked file. A multi-select fans out here, never at
  /// the engine — sending only the first of several files the user chose is
  /// silent data loss.
  Future<void> sendFile(
    String peerId,
    PeerTarget peer,
    String path, {
    String? name,
    int? size,
  }) async {
    if (path.isEmpty) return;
    final id = 'local-${++_optimisticSeq}';
    (_byPeer[peerId] ??= <ChatMessage>[]).add(
      ChatMessage(
        id: id,
        peerId: peerId,
        direction: 'out',
        // A file row carries no text — the engine persists an empty body too.
        body: '',
        at: DateTime.now(),
        // `staging`, matching what `begin_file_send` persists synchronously:
        // nothing has been copied yet, let alone sent. Claiming `transferring`
        // here would render "Sending…" over a file the engine has not read a
        // byte of, and would hide the Cancel this row is entitled to.
        status: ChatStatusValue.staging,
        kind: ChatMessageKind.file,
        fileName: name ?? _basename(path),
        fileSize: size,
        localPath: path,
      ),
    );
    notifyListeners();
    try {
      await _api?.chatSendFile(peer, path);
      await refresh(peerId);
      // Same reason as [send]: a file queued for a peer that never turns up
      // must still put its thread on the Conversations list.
      unawaited(refreshConversations());
    } on PeerBeamException catch (e) {
      // The engine refused the path itself (missing file, a folder): nothing
      // was persisted and nothing was sent, so a `refresh` here would silently
      // erase the row the user is looking at. Fail it in place instead, with
      // the engine's own reason.
      _fail(peerId, id, e.message);
    } catch (_) {
      // Anything else (a malformed reply, a dead engine): still the user's
      // row to account for. This call is fire-and-forget from the attach
      // button, so an escaping error would be an unhandled async one.
      _fail(peerId, id, 'Could not share ${name ?? _basename(path)}');
    }
  }

  /// Mark a message failed in place, remember why, and — because the engine
  /// persisted nothing for it — remember the row itself so no later reconcile
  /// can quietly erase the fact that it never left.
  void _fail(String peerId, String messageId, String reason) {
    _errors[messageId] = reason;
    final list = _byPeer[peerId];
    final i = list?.indexWhere((m) => m.id == messageId) ?? -1;
    if (list != null && i >= 0) {
      final failed = list[i].copyWith(status: ChatStatusValue.failed);
      list[i] = failed;
      (_unsent[peerId] ??= <ChatMessage>[]).add(failed);
    }
    notifyListeners();
  }

  static String _basename(String path) {
    final norm = path.replaceAll('\\', '/');
    final i = norm.lastIndexOf('/');
    return i >= 0 ? norm.substring(i + 1) : norm;
  }

  void _onReceived(ChatMessage m) {
    (_byPeer[m.peerId] ??= <ChatMessage>[]).add(m);
    notifyListeners();
    // A record can create a conversation that did not exist a moment ago —
    // which is exactly the thread the Conversations list exists to surface —
    // and an inbound file offer changes what that list says is waiting on the
    // user. Both need the summary re-read.
    unawaited(refreshConversations());
  }

  /// Flip a message's status in place: `pending` → `sent` once the engine's
  /// outbox delivers a text message, or the terminal status a shared file's
  /// transfer settled on. A safe no-op when the peer or message id isn't known
  /// locally — e.g. a stale/late status event for a conversation this session
  /// never loaded.
  ///
  /// This is the ONLY thing that drives a chat row's status. The engine
  /// deliberately gates which rows a transfer may settle (right conversation,
  /// right row, still in flight) before emitting this, so deriving statuses
  /// here from raw `transfer_*` events as well would be a second, ungated path
  /// to the same state — the live [TransferRepository] entry is used purely to
  /// overlay progress on an in-flight row.
  void _onStatus(ChatStatus e) {
    // A status without a reason supersedes any earlier one: the row moved on,
    // so a stale explanation must not stay under it.
    final reason = e.error;
    if (reason != null) {
      _errors[e.messageId] = reason;
    } else {
      _errors.remove(e.messageId);
    }
    // Staging progress, before the row-lookup below: these events arrive for a
    // share whose optimistic row still carries a local id, so the fraction has
    // to be kept whether or not a row can be found for it yet. Anything that is
    // NOT staging retires the bar — a bare `staging` event (the engine emits
    // one before the first byte) leaves whatever is already known, so the bar
    // never jumps back to indeterminate mid-copy.
    final progress = e.progress;
    if (progress != null) {
      _staging[e.messageId] = progress;
    } else if (e.status != ChatStatusValue.staging) {
      _staging.remove(e.messageId);
    }
    // The conversation summary can move here too (an offer accepted or
    // declined changes what is waiting on the user), but a staging tick cannot
    // — and there are ~100 of those per share, each of which would otherwise
    // cost a full scan of every conversation.
    if (progress == null && e.status != ChatStatusValue.staging) {
      unawaited(refreshConversations());
    }
    final list = _byPeer[e.peerId];
    if (list == null) return;
    final i = list.indexWhere((m) => m.id == e.messageId);
    if (i < 0) return;
    final updated = list[i].copyWith(status: e.status);
    list[i] = updated;
    notifyListeners();
    // A received file's saved location is written onto the persisted record
    // just before the row settles, and is not carried by this event — re-read
    // the conversation so the row can offer "Open". Narrow on purpose: no
    // other status transition adds anything a re-read would find.
    if (updated.isFile && e.status == ChatStatusValue.received) {
      unawaited(refresh(e.peerId));
    }
  }

  @override
  void dispose() {
    _disposed = true;
    _sub?.cancel();
    super.dispose();
  }
}
