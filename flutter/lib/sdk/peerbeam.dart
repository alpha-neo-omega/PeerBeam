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
import 'ffi/off_isolate.dart';
import 'models.dart';

/// Expected native ABI. The SDK refuses to run against a mismatched engine.
const int kExpectedAbi = 1;

/// The SDK surface. `PeerBeam` is the real (FFI) implementation; tests use a
/// fake. Methods are async to keep the door open for isolate offloading and to
/// give repositories a uniform await-able API.
abstract class PeerBeamApi {
  /// True when the native engine is loaded and usable.
  bool get available;

  /// The engine's own semantic version, or null when no engine is loaded.
  ///
  /// Read from the engine rather than duplicated in Dart: the app and the
  /// engine ship together, and a hand-maintained copy in the UI is a claim
  /// nobody re-checks. The About screen showed `0.3.0` for three releases
  /// because of exactly that, under a comment asking the next person to keep
  /// it in sync.
  String? get engineVersion;

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

  /// Accept an incoming transfer, this once.
  ///
  /// [confirmed] answers the engine's first-contact pairing check: it means the
  /// user compared this session's pairing code against the *other device's*
  /// screen and they match. The engine consults it only when the sender was
  /// pinned by that very handshake and `require_pairing_confirmation` is on;
  /// unconfirmed there, the accept is refused and the transfer stays pending.
  ///
  /// It defaults to false and must never be passed on anything but a real
  /// answer from the user. Passing true "because the prompt was shown" would
  /// turn the one check that detects a man-in-the-middle into a formality.
  Future<void> accept(String id, {bool confirmed = false});

  /// Accept AND trust the sending device: future transfers from it are
  /// auto-accepted whenever auto-accept is enabled. A plain [accept] never
  /// does this — trusting a device is always a separate, explicit choice.
  ///
  /// [confirmed] means what it does on [accept], and the engine gates this the
  /// same way — more strictly if anything, since this is the call that grants
  /// standing auto-accept.
  Future<void> acceptTrust(String id, {bool confirmed = false});
  Future<void> reject(String id);

  Future<List<TransferSnapshot>> activeTransfers();

  /// Transfers whose checkpoint outlived them — interrupted by a dropped link
  /// or a closed app, still resumable (outgoing) or still waiting for their
  /// sender (incoming).
  ///
  /// A separate call from [activeTransfers] on purpose: none of these are
  /// running, and folding them into one list would make "is this alive?" a
  /// field a caller has to remember to read.
  Future<List<InterruptedTransfer>> interruptedTransfers();

  /// Restart an interrupted **outgoing** transfer from its checkpoint.
  ///
  /// Not [resume], which un-pauses a live transfer. `peer` says only *how* to
  /// reach the device — its id must be the checkpoint's, so this can never be
  /// redirected at a different one; omit it and the engine uses discovery.
  /// Throws when the checkpoint no longer binds to its transfer (a different
  /// peer, a changed file) or when the transfer is inbound.
  Future<void> resumeInterrupted(String id, {PeerTarget? peer});

  /// Forget an interrupted transfer and the partial bytes it was holding.
  Future<void> discardInterrupted(String id);

  Future<List<HistoryEntry>> history();

  /// Clear all transfer history (persisted).
  Future<void> historyClear();

  /// Persisted engine settings (raw key/value document).
  Future<Map<String, dynamic>> settingsGet();

  /// Merge a partial settings object into the persisted document.
  Future<void> settingsSet(Map<String, dynamic> partial);

  /// Live device presence: what each trusted peer has shared, plus whether
  /// *this* device is sharing anything.
  ///
  /// Nothing here is persisted — presence is live state, so a fresh engine
  /// starts empty rather than showing stale numbers as current.
  Future<PresenceSnapshot> presence();

  /// Push a platform-supplied battery reading down to the engine (Android,
  /// whose `BatteryManager` the Rust platform layer cannot reach).
  ///
  /// Changes only *what* would be shared. It does not share anything: the
  /// opt-in setting and the trusted-only gate are untouched.
  Future<void> presenceBattery({int? percent, bool? charging});

  /// Offer the clipboard to [peers]. Returns how many pushes were queued.
  ///
  /// Naming a peer here does **not** send to it: the engine still decides per
  /// peer, after the handshake, against the trust store and the peer's
  /// negotiated capability. A `0` with the opt-in off means nothing was dialed
  /// at all — off is silent, not merely undelivered.
  ///
  /// Throws `invalid_argument` for an empty or over-cap clip, before anything
  /// is dialed, so a surface can say "too large to sync". An over-cap clip is
  /// never truncated.
  Future<int> clipboardSync(String text, List<PeerTarget> peers);

  /// Pinned (trusted) devices, newest first.
  Future<List<TrustedDevice>> trustList();

  /// Approve a **pinned** device without waiting for a transfer from it.
  ///
  /// Returns whether the device was pinned — `false` means this machine holds
  /// no key for it, so there is nothing to vouch for. Approval refines a TOFU
  /// pin and cannot create one: a device that has never completed a handshake
  /// has never presented a key.
  ///
  /// [share] chooses the initial permissions. `true` grants the frozen approval
  /// set (files, chat, clipboard, presence, pipe — never `notes` or `browse`,
  /// which stay opt-in). `false` is **trust without sharing**: the key is
  /// vouched for and nothing is granted.
  ///
  /// Either way the set is written only on the transition to approved, so
  /// calling this on an already-approved device cannot widen or narrow what it
  /// may do.
  Future<bool> trustApprove(String id, {bool share = true});

  /// Revoke a pinned device. Returns whether it was pinned.
  Future<bool> trustRemove(String id);

  /// Grant or withhold one per-device permission. Returns whether the store
  /// changed (`false` when it already read that way, which is not an error, so
  /// re-asserting a toggle is idempotent).
  ///
  /// `permission` is a [PeerBeamPermission] name. Takes effect on the device's
  /// **next** operation, not its next connection: every engine gate re-reads
  /// the trust store per message, clip, heartbeat and accept.
  ///
  /// Throws `invalid_argument` for a name this engine does not know — a surface
  /// built against a newer engine is told rather than silently ignored.
  Future<bool> trustSetPermission(String id, String permission, bool granted);

  /// Stop asking about this device's files, or start asking again. Returns
  /// whether the store changed.
  ///
  /// A prompt setting, not a permission: the engine consults it only after the
  /// `files` permission has already admitted the transfer, so this can never
  /// accept what would otherwise be refused. Setting it on a device that may
  /// not send files is inert.
  Future<bool> trustSetAutoAccept(String id, bool autoAccept);

  /// Replace the ordered auto-save rule list. Returns how many were stored.
  ///
  /// **The whole list at once**, because the order *is* the tie-break: the
  /// first rule that matches a received file chooses its directory. Adding,
  /// removing and reordering are all this one call, so a half-applied edit can
  /// never be persisted.
  ///
  /// Rules decide **where** an accepted file lands, never **whether** it is
  /// accepted. Every rule is validated by the engine — absolute destination, no
  /// `..`, an existing parent — and **one bad rule refuses the whole write**,
  /// throwing [InvalidArgumentException] with the offending rule's index.
  /// Throws [UnsupportedPlatformException] on Android, which cannot write to an
  /// arbitrary absolute path.
  ///
  /// Read them back through [settingsGet] (`save_rules`), alongside
  /// `rules_supported`.
  Future<int> rulesSet(List<SaveRule> rules);

  /// Send a chat text message to a peer. Returns the new message id.
  /// Send a text message, optionally answering [inReplyTo].
  ///
  /// The reply carries only the answered message's **id**. Nothing quotes text
  /// into the new body: a snapshot would survive the message it quoted, which
  /// is how a disappearing-message window gets defeated by replying to
  /// something.
  Future<String> chatSend(PeerTarget peer, String text, {String? inReplyTo});

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

  /// Search this device's stored conversations for [query], newest match
  /// first.
  ///
  /// **A pure local read.** It never dials, never opens a channel, and never
  /// depends on a peer being online — a thread whose device is long gone is
  /// searchable exactly like one that is here. A conversation the user deleted
  /// is not searchable at all: its rows are gone, and there is nowhere for them
  /// to come back from.
  ///
  /// [query] matches **case-insensitively** as a plain substring of a message's
  /// text or a shared file's *name*. It is not a regular expression, and a
  /// file's path on this device is never searched — that is where the file
  /// happens to sit on disk, not anything anyone said. An empty or
  /// whitespace-only query finds nothing rather than everything.
  ///
  /// [limit] bounds the results (the engine's default when omitted; at most
  /// 500). Whatever it is, check [ChatSearchResults.truncated] and show it —
  /// silently returning the first `n` reads as "that is all there is", which
  /// for a search over the user's own history is a wrong answer rather than a
  /// partial one.
  ///
  /// The search runs in the engine and not here, deliberately: a filter in Dart
  /// would mean loading every message of every conversation across the FFI to
  /// answer one query.
  Future<ChatSearchResults> chatSearch(String query, {int? limit});

  /// React to a message with [emoji], or withdraw that reaction.
  ///
  /// Returns `(applied, delivered)`. They are separate answers on purpose:
  /// `applied` says this device's own history changed, `delivered` says the
  /// peer was told. A peer that is offline — or too old to have negotiated
  /// reactions — leaves `delivered` false rather than failing the call, so a
  /// surface must read `delivered` before showing the gesture as seen.
  ///
  /// Reactions are not queued for later delivery.
  Future<({bool applied, bool delivered})> chatReact(
    String peerId,
    String messageId,
    String emoji, {
    bool remove = false,
  });

  /// Tell [peerId] we have read its messages up to and including
  /// [readThrough], and report whether anything was actually sent.
  ///
  /// `false` is the ordinary answer, not a failure: this device sends no
  /// receipts at all unless the user has opted in (**default off**), and the
  /// peer may be offline or too old to understand them. Reading a conversation
  /// is never, by itself, consent to report having read it.
  Future<bool> chatMarkRead(String peerId, String readThrough);

  /// Every live note, newest edit first. Deleted notes are never returned.
  Future<List<Note>> notesList();

  /// Create a note, returning its id.
  Future<String> notesCreate(String body, {String title = ''});

  /// Replace a note's content. `false` means no such note, or one that has been
  /// deleted — editing a tombstone would resurrect it.
  Future<bool> notesEdit(String id, String body, {String title = ''});

  /// Delete a note, leaving a tombstone so the deletion can reach other
  /// devices. `false` means there was nothing to delete.
  Future<bool> notesDelete(String id);

  /// Exchange notes with [peer], returning whether anything was sent.
  ///
  /// `false` is a normal answer: the device may not have been granted the
  /// `notes` permission, may be unreachable, or may run a build without notes.
  Future<bool> notesSync(PeerTarget peer);

  /// Ask [peer] to make itself findable for [seconds].
  ///
  /// The returned value says only that the request went out. Whether the device
  /// rings is its own decision, taken against the presence permission it holds
  /// for this machine, and it deliberately never answers — a caller told
  /// "refused" could map which devices are listening.
  Future<bool> presenceRing(PeerTarget peer, {int seconds = 15});

  /// Ask [peer] what is in one of its shared folders.
  ///
  /// [path] is share-relative — `photos/2026`, never absolute. Empty asks what
  /// the device shares at all.
  Future<BrowseListing> browse(PeerTarget peer, {String path = ''});

  /// Sync a folder with a device, **both ways**.
  ///
  /// Files only they changed are fetched, files only you changed are pushed,
  /// and their deletions are applied when they follow from your copy. When both
  /// sides changed a file, their copy arrives beside yours under a
  /// `.sync-conflict-` name and **yours is untouched** — no automatic rule can
  /// pick correctly, and every one of them loses somebody's work.
  ///
  /// Returns once the work is planned and requested; bytes arrive as ordinary
  /// inbound transfers through the usual approval path.
  Future<SyncResult> syncFolder(PeerTarget peer, String path, String into);

  /// The folders this device offers to peers.
  ///
  /// **Empty by default** — nothing is shared until someone chooses it — and a
  /// peer still needs the Browse permission to see any of it.
  Future<List<SharedFolder>> sharedFolders();

  /// Replace the set of shared folders. Takes effect immediately, because
  /// un-sharing a folder that stays shared until the next restart is the one
  /// direction this must never lag in.
  Future<void> setSharedFolders(List<String> paths);

  /// This device's recent log lines, newest last.
  ///
  /// Bounded and in-memory: logs are for diagnosing the session you are in.
  Future<List<LogLine>> logs({int limit});

  /// Ask whether a newer release exists. **Only ever when a person asks.**
  ///
  /// The one outbound request this app makes to anything but a peer, permitted
  /// by amendment A1 on terms every caller has to keep: never on a timer or at
  /// launch, no identifiers sent, and the answer is shown rather than acted on.
  /// An unreachable feed comes back as [UpdateCheck.reachable] false, not as a
  /// thrown error — offline is ordinary here.
  Future<UpdateCheck> checkForUpdates();

  /// The groups this device is in, and the invitations waiting for an answer.
  ///
  /// Both in one call because an invitation is the only way into a group: one
  /// that arrived while the app was closed would otherwise stay invisible.
  Future<GroupsView> groups();

  /// Create a group holding only this device.
  ///
  /// Members are invited and join when **they** accept — nothing here can add
  /// somebody else's device to a group they never agreed to.
  Future<Group> createGroup(String name);

  /// Rename a group **on this device only**. Names are never shared.
  Future<Group> renameGroup(String id, String name);

  /// Turn an invitation down. Local and silent — the inviter is not told.
  Future<bool> declineGroupInvite(String group);

  /// Offer [peer] a place in [id].
  ///
  /// An offer, not an enrolment. The roster travels with it, so they see who is
  /// already in the group — and everyone in it sees them if they accept.
  /// **Say so before calling this.**
  ///
  /// Returns once queued; the dial happens in the background so the calling
  /// isolate is never blocked.
  Future<void> inviteToGroup(String id, PeerTarget peer);

  /// Accept an invitation.
  ///
  /// [peers] is how to reach the members — the engine holds ids, the app holds
  /// routes. A member that cannot be reached now is told at next contact.
  Future<Group> acceptGroupInvite(String group, List<PeerTarget> peers);

  /// Leave a group: tell whoever can be reached, then forget it here.
  ///
  /// Forgotten whether or not anyone heard. A member that missed the message
  /// may keep sending; withhold `chat` from that device to refuse it.
  Future<void> leaveGroup(String id, List<PeerTarget> peers);

  /// Send [text] to every member this device may message.
  ///
  /// Returns the shared message id and the members that were **skipped**, named
  /// rather than silently dropped.
  Future<GroupSendResult> sendToGroup(String id, String text);

  /// A group's messages, gathered across its members.
  Future<List<ChatMessage>> groupHistory(String group);

  /// Every Space this device keeps.
  Future<List<Space>> spaces();

  /// Create a Space. Names are unique ignoring case.
  Future<Space> createSpace(String name);

  /// Rename a Space.
  Future<Space> renameSpace(String id, String name);

  /// Delete a Space. The devices in it keep their trust.
  Future<bool> deleteSpace(String id);

  /// Add a trusted device. Grants nothing: every send still passes the same
  /// per-capability check a hand-typed one does.
  Future<bool> addSpaceMember(String id, String device);

  /// Remove a device from a Space.
  Future<bool> removeSpaceMember(String id, String device);

  /// Mark a device as one of the user's own, or unmark it.
  ///
  /// A local label. It widens no permission and the device is never told.
  Future<bool> setDeviceMine(String device, {required bool mine});

  /// The devices the user marked as their own.
  Future<List<String>> myDevices();

  /// The hardware address recorded for a device, or null when there is none.
  ///
  /// A write-only setting is one nobody keeps using: the address could be
  /// stored and sent with and never read back, so checking one meant typing it
  /// again — and an address typed from memory is one typed wrong.
  Future<String?> wakeAddress(String device);

  /// Record where to send a wake packet.
  Future<String> setWakeAddress(String device, String mac);

  /// Forget a device's hardware address.
  Future<bool> forgetWakeAddress(String device);

  /// Send a wake packet. **Local network only**, and it reports what was sent
  /// rather than that the device woke — the protocol has no reply.
  Future<WakeAttempt> wakeDevice(String device);

  /// This conversation's disappearing-message window in seconds, or null when
  /// messages are kept until deleted.
  Future<int?> chatRetention(String peerId);

  /// Set the window, or pass null to turn it off.
  ///
  /// **Local.** Nothing is sent to the peer, and no surface may suggest the
  /// peer's copy is affected.
  Future<int?> setChatRetention(String peerId, int? seconds);

  /// Delete messages whose window has closed. Omit [peerId] for every
  /// conversation.
  Future<({int messages, int queued})> pruneChat({String? peerId});

  /// Copy the buffered logs to a file and return where they went.
  ///
  /// Pass [path] to choose the destination; omit it for a temporary file. Use
  /// this for a bug report rather than asking someone to find a log directory.
  Future<String> exportLogs({String? path});

  /// Start or stop receiving `log_received` events as they happen.
  Future<void> subscribeLogs(bool enabled);

  /// Keep syncing [path] into [into] until [unwatchFolder] is called.
  ///
  /// A file is only acted on once it has stopped changing, so saving a large
  /// file mid-poll never syncs a half-written copy.
  Future<void> watchFolder(
    PeerTarget peer,
    String path,
    String into, {
    int intervalSeconds,
  });

  /// Stop watching a folder.
  Future<void> unwatchFolder(String path, String into);

  /// Which folders are currently being watched.
  Future<List<WatchedFolder>> watchedFolders();

  /// This device's activity, newest first. A pure local read.
  Future<List<TimelineEvent>> timeline({int? limit});

  /// Every remembered clip, newest first.
  ///
  /// Empty unless clipboard history is on. Reading is never gated — an empty
  /// list is the honest answer for a device that records nothing.
  Future<List<ClipEntry>> clipboardHistory();

  /// Forget every remembered clip, returning how many were removed. Works
  /// whether or not the setting is on.
  Future<int> clipboardHistoryClear();

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
  // `prefer_initializing_formals` suggests `this._invokeOff`, which Dart does
  // not allow: a named parameter may not begin with an underscore, and the
  // field is private on purpose.
  PeerBeam({String? overrideLibPath, OffIsolateInvoke? invokeOff})
    : _libPath = overrideLibPath,
      // ignore: prefer_initializing_formals
      _invokeOff = invokeOff {
    try {
      _b = Bindings.load(overridePath: overrideLibPath);
    } on NativeLoadError {
      _b = null;
    }
  }

  /// Forwarded to the background isolate so it opens the same library file.
  final String? _libPath;

  /// Substituted by tests; `null` means the real isolate hop.
  final OffIsolateInvoke? _invokeOff;

  /// Run one blocking native call off the UI isolate.
  ///
  /// Used only by the calls that dial a peer. Everything else reads a local
  /// store and returns in microseconds, where an isolate hop would cost more
  /// than the work it moved.
  ///
  /// Availability is checked here, on the UI isolate, so an absent engine
  /// throws [PeerBeamUnavailable] exactly as it did before rather than failing
  /// later as a library-load error inside a spawned isolate — a place the
  /// caller has no way to catch it from.
  ///
  /// Only for the real path. A substituted [OffIsolateInvoke] *is* the engine
  /// for that call, so demanding a loaded library as well would make the seam
  /// untestable anywhere the library is absent, which is everywhere CI runs.
  Future<Map<String, dynamic>> _off(String symbol, [String? arg]) async {
    final invoke = _invokeOff;
    if (invoke == null) _req();
    final raw = invoke != null
        ? await invoke(symbol, arg)
        : await invokeOffIsolate(symbol, arg, _libPath);
    // Decoded on this isolate, so a refusal arrives as the same
    // `PeerBeamException` subclass every caller already catches.
    return _data(raw);
  }

  @override
  bool get available => _b != null;

  @override
  String? get engineVersion {
    final b = _b;
    if (b == null) return null;
    try {
      final decoded = jsonDecode(b.versionJson());
      if (decoded is! Map) return null;
      final data = decoded['data'];
      final semver = (data is Map ? data['semver'] : null) ?? decoded['semver'];
      return semver is String && semver.isNotEmpty ? semver : null;
    } catch (_) {
      // A version string is never worth failing a screen over.
      return null;
    }
  }

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
  Future<void> accept(String id, {bool confirmed = false}) async =>
      _data(_req().accept(_decision(id, confirmed)));
  @override
  Future<void> acceptTrust(String id, {bool confirmed = false}) async =>
      _data(_req().acceptTrust(_decision(id, confirmed)));
  @override
  Future<void> reject(String id) async => _data(_req().reject(_id(id)));

  @override
  Future<List<TransferSnapshot>> activeTransfers() async {
    final data = _data(_req().active());
    return _list(data['transfers']).map(TransferSnapshot.fromJson).toList();
  }

  @override
  Future<List<InterruptedTransfer>> interruptedTransfers() async {
    final data = _data(_req().interrupted());
    return _list(data['transfers']).map(InterruptedTransfer.fromJson).toList();
  }

  @override
  Future<void> resumeInterrupted(String id, {PeerTarget? peer}) async => _data(
    _req().resumeInterrupted(
      jsonEncode({'id': id, if (peer != null) 'peer': peer.toJson()}),
    ),
  );

  @override
  Future<void> discardInterrupted(String id) async =>
      _data(_req().discardInterrupted(_id(id)));

  @override
  Future<Map<String, dynamic>> settingsGet() async =>
      _data(_req().settingsGet());

  @override
  Future<void> settingsSet(Map<String, dynamic> partial) async =>
      _data(_req().settingsSet(jsonEncode(partial)));

  @override
  Future<PresenceSnapshot> presence() async =>
      PresenceSnapshot.fromJson(_data(_req().presence()));

  @override
  Future<void> presenceBattery({int? percent, bool? charging}) async => _data(
    _req().presenceBattery(
      jsonEncode({'percent': ?percent, 'charging': ?charging}),
    ),
  );

  @override
  Future<int> clipboardSync(String text, List<PeerTarget> peers) async {
    final data = _data(
      _req().clipboardSync(
        jsonEncode({
          'text': text,
          'peers': peers.map((p) => p.toJson()).toList(),
        }),
      ),
    );
    return (data['queued'] as num?)?.toInt() ?? 0;
  }

  @override
  Future<List<TrustedDevice>> trustList() async {
    final data = _data(_req().trustList());
    return _list(data['devices']).map(TrustedDevice.fromJson).toList();
  }

  @override
  Future<bool> trustApprove(String id, {bool share = true}) async {
    final data = _data(
      _req().trustApprove(jsonEncode({'id': id, 'share': share})),
    );
    return data['pinned'] == true;
  }

  @override
  Future<bool> trustRemove(String id) async {
    final data = _data(_req().trustRemove(jsonEncode({'id': id})));
    return data['removed'] == true;
  }

  @override
  Future<bool> trustSetPermission(
    String id,
    String permission,
    bool granted,
  ) async {
    final data = _data(
      _req().trustSetPermission(
        jsonEncode({'id': id, 'permission': permission, 'granted': granted}),
      ),
    );
    return data['changed'] == true;
  }

  @override
  Future<bool> trustSetAutoAccept(String id, bool autoAccept) async {
    final data = _data(
      _req().trustSetAutoAccept(
        jsonEncode({'id': id, 'auto_accept': autoAccept}),
      ),
    );
    return data['changed'] == true;
  }

  @override
  Future<int> rulesSet(List<SaveRule> rules) async {
    final data = _data(
      _req().rulesSet(
        jsonEncode({'rules': rules.map((r) => r.toJson()).toList()}),
      ),
    );
    return (data['count'] as num?)?.toInt() ?? 0;
  }

  @override
  Future<List<HistoryEntry>> history() async {
    final data = _data(_req().history());
    return _list(data['history']).map(HistoryEntry.fromJson).toList();
  }

  @override
  Future<void> historyClear() async => _data(_req().historyClear());

  @override
  Future<String> chatSend(
    PeerTarget peer,
    String text, {
    String? inReplyTo,
  }) async {
    final data = _data(
      _req().chatSend(
        jsonEncode({
          'peer': peer.toJson(),
          'text': text,
          'in_reply_to': ?inReplyTo,
        }),
      ),
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
  Future<ChatSearchResults> chatSearch(String query, {int? limit}) async {
    // `limit` is omitted rather than sent as null when the caller did not ask
    // for one: the engine refuses an out-of-range limit rather than clamping
    // it, and "no opinion" must reach it as no key at all.
    final data = _data(
      _req().chatSearch(jsonEncode({'query': query, 'limit': ?limit})),
    );
    return ChatSearchResults(
      hits: _list(data['hits']).map(ChatSearchHit.fromJson).toList(),
      truncated: data['truncated'] == true,
      limit: (data['limit'] as num?)?.toInt() ?? 0,
    );
  }

  @override
  Future<({bool applied, bool delivered})> chatReact(
    String peerId,
    String messageId,
    String emoji, {
    bool remove = false,
  }) async {
    final data = await _off(
      'pb_chat_react',
      jsonEncode({
        'peer': peerId,
        'id': messageId,
        'emoji': emoji,
        'remove': remove,
      }),
    );
    return (
      applied: data['applied'] == true,
      delivered: data['delivered'] == true,
    );
  }

  @override
  Future<bool> chatMarkRead(String peerId, String readThrough) async {
    final data = await _off(
      'pb_chat_mark_read',
      jsonEncode({'peer': peerId, 'read_through': readThrough}),
    );
    return data['sent'] == true;
  }

  @override
  Future<List<Note>> notesList() async {
    final data = _data(_req().notesList('{}'));
    return _list(data['notes']).map(Note.fromJson).toList();
  }

  @override
  Future<String> notesCreate(String body, {String title = ''}) async {
    final data = _data(
      _req().notesCreate(jsonEncode({'title': title, 'body': body})),
    );
    return data['id'] as String;
  }

  @override
  Future<bool> notesEdit(String id, String body, {String title = ''}) async {
    final data = _data(
      _req().notesEdit(jsonEncode({'id': id, 'title': title, 'body': body})),
    );
    return data['updated'] == true;
  }

  @override
  Future<bool> notesDelete(String id) async {
    final data = _data(_req().notesDelete(jsonEncode({'id': id})));
    return data['deleted'] == true;
  }

  @override
  Future<bool> notesSync(PeerTarget peer) async {
    final data = await _off(
      'pb_notes_sync',
      jsonEncode({'peer': peer.toJson()}),
    );
    return data['sent'] == true;
  }

  @override
  Future<bool> presenceRing(PeerTarget peer, {int seconds = 15}) async {
    final data = await _off(
      'pb_presence_ring',
      jsonEncode({'peer': peer.toJson(), 'seconds': seconds}),
    );
    return data['sent'] == true;
  }

  @override
  Future<BrowseListing> browse(PeerTarget peer, {String path = ''}) async {
    final data = await _off(
      'pb_browse_list',
      jsonEncode({'peer': peer.toJson(), 'path': path}),
    );
    return BrowseListing.fromJson(data);
  }

  @override
  Future<List<SharedFolder>> sharedFolders() async {
    final data = _data(_req().browseShares('{}'));
    final list = (data['entries'] as List?) ?? const [];
    return list
        .map((e) => SharedFolder.fromJson(e as Map<String, dynamic>))
        .toList();
  }

  @override
  Future<void> setSharedFolders(List<String> paths) async {
    await settingsSet({'shared_directories': paths});
  }

  @override
  Future<UpdateCheck> checkForUpdates() async =>
      UpdateCheck.fromJson(await _off('pb_check_updates'));

  @override
  Future<List<Space>> spaces() async {
    final data = _data(_req().spacesList('{}'));
    return ((data['spaces'] as List?) ?? const [])
        .map((e) => Space.fromJson(e as Map<String, dynamic>))
        .toList();
  }

  @override
  Future<Space> createSpace(String name) async => Space.fromJson(
    _data(_req().spacesCreate(jsonEncode({'name': name})))['space']
        as Map<String, dynamic>,
  );

  @override
  Future<Space> renameSpace(String id, String name) async => Space.fromJson(
    _data(_req().spacesRename(jsonEncode({'id': id, 'name': name})))['space']
        as Map<String, dynamic>,
  );

  @override
  Future<bool> deleteSpace(String id) async =>
      (_data(_req().spacesDelete(jsonEncode({'id': id})))['deleted']
          as bool?) ??
      false;

  @override
  Future<bool> addSpaceMember(String id, String device) async =>
      (_data(
            _req().spacesAddMember(jsonEncode({'id': id, 'device': device})),
          )['added']
          as bool?) ??
      false;

  @override
  Future<bool> removeSpaceMember(String id, String device) async =>
      (_data(
            _req().spacesRemoveMember(jsonEncode({'id': id, 'device': device})),
          )['removed']
          as bool?) ??
      false;

  @override
  Future<bool> setDeviceMine(String device, {required bool mine}) async =>
      (_data(
            _req().trustSetMine(jsonEncode({'device': device, 'mine': mine})),
          )['changed']
          as bool?) ??
      false;

  @override
  Future<List<String>> myDevices() async {
    final data = _data(_req().trustMyDevices('{}'));
    return ((data['devices'] as List?) ?? const [])
        .map((e) => (e as Map<String, dynamic>)['device'].toString())
        .toList();
  }

  @override
  Future<String?> wakeAddress(String device) async =>
      _data(_req().wakeGet(jsonEncode({'device': device})))['mac'] as String?;

  @override
  Future<String> setWakeAddress(String device, String mac) async =>
      (_data(_req().wakeSet(jsonEncode({'device': device, 'mac': mac})))['mac']
          as String?) ??
      mac;

  @override
  Future<bool> forgetWakeAddress(String device) async =>
      (_data(_req().wakeForget(jsonEncode({'device': device})))['forgotten']
          as bool?) ??
      false;

  @override
  Future<WakeAttempt> wakeDevice(String device) async => WakeAttempt.fromJson(
    _data(_req().wakeSend(jsonEncode({'device': device}))),
  );

  @override
  Future<int?> chatRetention(String peerId) async =>
      _data(_req().chatRetentionGet(jsonEncode({'peer_id': peerId})))['seconds']
          as int?;

  @override
  Future<int?> setChatRetention(String peerId, int? seconds) async =>
      _data(
            _req().chatRetentionSet(
              jsonEncode({'peer_id': peerId, 'seconds': ?seconds}),
            ),
          )['seconds']
          as int?;

  @override
  Future<({int messages, int queued})> pruneChat({String? peerId}) async {
    final data = _data(
      _req().chatPrune(jsonEncode(peerId == null ? {} : {'peer_id': peerId})),
    );
    return (
      messages: (data['messages'] as int?) ?? 0,
      queued: (data['queued'] as int?) ?? 0,
    );
  }

  @override
  Future<List<LogLine>> logs({int limit = 200}) async {
    final data = _data(_req().logsGet(jsonEncode({'limit': limit})));
    final list = (data['logs'] as List?) ?? const [];
    return list
        .map((e) => LogLine.fromJson(e as Map<String, dynamic>))
        .toList();
  }

  @override
  Future<String> exportLogs({String? path}) async {
    final data = _data(
      _req().logsExport(jsonEncode(path == null ? {} : {'path': path})),
    );
    return data['path'] as String? ?? '';
  }

  @override
  Future<void> subscribeLogs(bool enabled) async {
    _data(_req().logsSubscribe(jsonEncode({'enabled': enabled})));
  }

  @override
  Future<SyncResult> syncFolder(
    PeerTarget peer,
    String path,
    String into,
  ) async {
    final data = await _off(
      'pb_sync_pull',
      jsonEncode({'peer': peer.toJson(), 'path': path, 'into': into}),
    );
    return SyncResult.fromJson(data);
  }

  @override
  Future<void> watchFolder(
    PeerTarget peer,
    String path,
    String into, {
    int intervalSeconds = 30,
  }) async {
    _data(
      _req().syncWatch(
        jsonEncode({
          'peer': peer.toJson(),
          'path': path,
          'into': into,
          'interval': intervalSeconds,
        }),
      ),
    );
  }

  @override
  Future<void> unwatchFolder(String path, String into) async {
    _data(_req().syncUnwatch(jsonEncode({'path': path, 'into': into})));
  }

  @override
  Future<List<WatchedFolder>> watchedFolders() async {
    final data = _data(_req().syncWatches('{}'));
    final list = (data['watching'] as List?) ?? const [];
    return list
        .map((e) => WatchedFolder.fromJson(e as Map<String, dynamic>))
        .toList();
  }

  @override
  Future<List<TimelineEvent>> timeline({int? limit}) async {
    final data = _data(_req().timeline(jsonEncode({'limit': ?limit})));
    return _list(data['events']).map(TimelineEvent.fromJson).toList();
  }

  @override
  Future<List<ClipEntry>> clipboardHistory() async {
    final data = _data(_req().clipHistory('{}'));
    return _list(data['entries']).map(ClipEntry.fromJson).toList();
  }

  @override
  Future<int> clipboardHistoryClear() async {
    final data = _data(_req().clipHistoryClear('{}'));
    return (data['cleared'] as num?)?.toInt() ?? 0;
  }

  @override
  Future<GroupsView> groups() async {
    final data = _data(_req().groupsList('{}'));
    return GroupsView(
      groups: _list(data['groups']).map(Group.fromJson).toList(),
      invites: _list(data['invites']).map(GroupInvite.fromJson).toList(),
    );
  }

  @override
  Future<Group> createGroup(String name) async {
    final data = _data(_req().groupsCreate(jsonEncode({'name': name})));
    return Group.fromJson(data['group'] as Map<String, dynamic>);
  }

  @override
  Future<Group> renameGroup(String id, String name) async {
    final data = _data(
      _req().groupsRename(jsonEncode({'id': id, 'name': name})),
    );
    return Group.fromJson(data['group'] as Map<String, dynamic>);
  }

  @override
  Future<bool> declineGroupInvite(String group) async {
    final data = _data(_req().groupsDecline(jsonEncode({'group': group})));
    return data['declined'] == true;
  }

  @override
  Future<void> inviteToGroup(String id, PeerTarget peer) async {
    _data(_req().groupsInvite(jsonEncode({'id': id, 'peer': peer.toJson()})));
  }

  @override
  Future<Group> acceptGroupInvite(String group, List<PeerTarget> peers) async {
    final data = _data(
      _req().groupsAccept(
        jsonEncode({
          'group': group,
          'peers': peers.map((p) => p.toJson()).toList(),
        }),
      ),
    );
    return Group.fromJson(data['group'] as Map<String, dynamic>);
  }

  @override
  Future<void> leaveGroup(String id, List<PeerTarget> peers) async {
    _data(
      _req().groupsLeave(
        jsonEncode({'id': id, 'peers': peers.map((p) => p.toJson()).toList()}),
      ),
    );
  }

  @override
  Future<GroupSendResult> sendToGroup(String id, String text) async {
    final data = _data(_req().groupsSend(jsonEncode({'id': id, 'text': text})));
    return GroupSendResult(
      id: data['id'] as String? ?? '',
      sent: (data['sent'] as num?)?.toInt() ?? 0,
      skipped: [...?(data['skipped'] as List<dynamic>?)?.whereType<String>()],
    );
  }

  @override
  Future<List<ChatMessage>> groupHistory(String group) async {
    final data = _data(_req().groupsHistory(jsonEncode({'group': group})));
    return _list(data['messages']).map(ChatMessage.fromJson).toList();
  }

  @override
  Future<List<ChatMessage>> chatHistory(String peerId) async {
    final data = _data(_req().chatHistory(jsonEncode({'peer_id': peerId})));
    return _list(data['messages']).map(ChatMessage.fromJson).toList();
  }

  @override
  Future<int> chatReconcile(String peerId) async {
    final data = _data(_req().chatReconcile(jsonEncode({'peer_id': peerId})));
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

  /// An accept request, carrying the user's pairing answer.
  ///
  /// `confirmed` is only ever sent when it is true. The engine accepts nothing
  /// but a literal `true`, so an omitted key and an explicit `false` mean the
  /// same thing to it — and sending the key only when there is something to say
  /// keeps every ordinary accept byte-identical to what it was before this
  /// check existed.
  String _decision(String id, bool confirmed) =>
      jsonEncode({'id': id, if (confirmed) 'confirmed': true});
}
