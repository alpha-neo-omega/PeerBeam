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
  final _RetDart _checkUpdates;
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
  final _ArgRetDart _acceptTrust;
  final _ArgRetDart _reject;
  final _RetDart _active;
  final _RetDart _interrupted;
  final _ArgRetDart _resumeInterrupted;
  final _ArgRetDart _discardInterrupted;
  final _ArgRetDart _get;
  final _RetDart _history;
  final _RetDart _trustList;
  final _ArgRetDart _trustApprove;
  final _ArgRetDart _trustRemove;
  final _ArgRetDart _trustSetPermission;
  final _ArgRetDart _trustSetAutoAccept;
  final _ArgRetDart _rulesSet;
  final _RetDart _historyClear;
  final _RetDart _settingsGet;
  final _ArgRetDart _settingsSet;
  final _ArgRetDart _chatSend;
  final _ArgRetDart _chatSendFile;
  final _ArgRetDart _chatHistory;
  final _ArgRetDart _chatReconcile;
  final _ArgRetDart _chatCancel;
  final _ArgRetDart _chatConversations;
  final _ArgRetDart _chatDelete;
  final _ArgRetDart _chatDeleteMessages;
  final _ArgRetDart _chatSearch;
  final _ArgRetDart _chatReact;
  final _ArgRetDart _chatMarkRead;
  final _ArgRetDart _notesList;
  final _ArgRetDart _groupsList;
  final _ArgRetDart _groupsCreate;
  final _ArgRetDart _groupsRename;
  final _ArgRetDart _groupsDecline;
  final _ArgRetDart _groupsInvite;
  final _ArgRetDart _groupsAccept;
  final _ArgRetDart _groupsLeave;
  final _ArgRetDart _groupsSend;
  final _ArgRetDart _groupsHistory;
  final _ArgRetDart _spacesList;
  final _ArgRetDart _spacesCreate;
  final _ArgRetDart _spacesRename;
  final _ArgRetDart _spacesDelete;
  final _ArgRetDart _spacesAddMember;
  final _ArgRetDart _spacesRemoveMember;
  final _ArgRetDart _trustSetMine;
  final _ArgRetDart _trustMyDevices;
  final _ArgRetDart _wakeSet;
  final _ArgRetDart _wakeGet;
  final _ArgRetDart _wakeForget;
  final _ArgRetDart _wakeSend;
  final _ArgRetDart _chatRetentionGet;
  final _ArgRetDart _chatRetentionSet;
  final _ArgRetDart _chatPrune;
  final _ArgRetDart _notesCreate;
  final _ArgRetDart _notesEdit;
  final _ArgRetDart _notesDelete;
  final _ArgRetDart _notesSync;
  final _ArgRetDart _presenceRing;
  final _ArgRetDart _clipHistory;
  final _ArgRetDart _clipHistoryClear;
  final _ArgRetDart _timeline;
  final _ArgRetDart _browseList;
  final _ArgRetDart _browseShares;
  final _ArgRetDart _logsGet;
  final _ArgRetDart _logsExport;
  final _ArgRetDart _logsSubscribe;
  final _ArgRetDart _syncPull;
  final _ArgRetDart _syncWatch;
  final _ArgRetDart _syncUnwatch;
  final _ArgRetDart _syncWatches;
  final _RetDart _presence;
  final _ArgRetDart _presenceBattery;
  final _ArgRetDart _clipboardSync;

  Bindings._(DynamicLibrary lib)
    : _abiVersion = lib.lookupFunction<_AbiC, _AbiDart>('pb_abi_version'),
      _versionJson = lib.lookupFunction<_RetC, _RetDart>('pb_version_json'),
      _checkUpdates = lib.lookupFunction<_RetC, _RetDart>('pb_check_updates'),
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
      _acceptTrust = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_transfer_accept_trust',
      ),
      _reject = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_transfer_reject'),
      _active = lib.lookupFunction<_RetC, _RetDart>('pb_transfers_active'),
      _interrupted = lib.lookupFunction<_RetC, _RetDart>(
        'pb_transfers_interrupted',
      ),
      // NOT `pb_transfer_resume`: that un-pauses a live transfer, this
      // restarts a dead one from its checkpoint. Two verbs, two symbols.
      _resumeInterrupted = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_transfer_resume_interrupted',
      ),
      _discardInterrupted = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_transfer_discard_interrupted',
      ),
      _get = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_transfer_get'),
      _history = lib.lookupFunction<_RetC, _RetDart>('pb_history_get'),
      _trustList = lib.lookupFunction<_RetC, _RetDart>('pb_trust_list'),
      _trustApprove = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_trust_approve',
      ),
      _trustRemove = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_trust_remove',
      ),
      _trustSetPermission = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_trust_set_permission',
      ),
      _trustSetAutoAccept = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_trust_set_auto_accept',
      ),
      _rulesSet = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_rules_set'),
      _historyClear = lib.lookupFunction<_RetC, _RetDart>('pb_history_clear'),
      _settingsGet = lib.lookupFunction<_RetC, _RetDart>('pb_settings_get'),
      _settingsSet = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_settings_set',
      ),
      _chatSend = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_chat_send'),
      _chatSendFile = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_chat_send_file',
      ),
      _chatHistory = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_chat_history',
      ),
      _chatReconcile = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_chat_reconcile',
      ),
      _chatCancel = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_chat_cancel'),
      // Takes no arguments (`{}` or null both mean "call"), but is still an
      // arg-taking C signature — the export reads its pointer with
      // `read_json_or_empty`.
      _chatConversations = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_chat_conversations',
      ),
      _chatDelete = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_chat_delete'),
      _chatDeleteMessages = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_chat_delete_messages',
      ),
      _chatSearch = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_chat_search'),
      _chatReact = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_chat_react'),
      _chatMarkRead = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_chat_mark_read',
      ),
      _notesList = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_notes_list'),
      _groupsList = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_groups_list'),
      _groupsCreate = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_groups_create',
      ),
      _groupsRename = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_groups_rename',
      ),
      _groupsDecline = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_groups_decline',
      ),
      _groupsInvite = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_groups_invite',
      ),
      _groupsAccept = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_groups_accept',
      ),
      _groupsLeave = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_groups_leave',
      ),
      _groupsSend = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_groups_send'),
      _groupsHistory = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_groups_history',
      ),
      _spacesList = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_spaces_list'),
      _spacesCreate = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_spaces_create',
      ),
      _spacesRename = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_spaces_rename',
      ),
      _spacesDelete = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_spaces_delete',
      ),
      _spacesAddMember = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_spaces_add_member',
      ),
      _spacesRemoveMember = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_spaces_remove_member',
      ),
      _trustSetMine = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_trust_set_mine',
      ),
      _trustMyDevices = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_trust_my_devices',
      ),
      _wakeSet = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_wake_set'),
      _wakeGet = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_wake_get'),
      _wakeForget = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_wake_forget'),
      _wakeSend = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_wake_send'),
      _chatRetentionGet = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_chat_retention_get',
      ),
      _chatRetentionSet = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_chat_retention_set',
      ),
      _chatPrune = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_chat_prune'),
      _notesCreate = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_notes_create',
      ),
      _notesEdit = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_notes_edit'),
      _notesDelete = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_notes_delete',
      ),
      _notesSync = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_notes_sync'),
      _presenceRing = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_presence_ring',
      ),
      _clipHistory = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_clipboard_history',
      ),
      _clipHistoryClear = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_clipboard_history_clear',
      ),
      _timeline = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_timeline'),
      _browseList = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_browse_list'),
      _browseShares = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_browse_shares',
      ),
      _logsGet = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_logs_get'),
      _logsExport = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_logs_export'),
      _logsSubscribe = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_logs_subscribe',
      ),
      _syncPull = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_sync_pull'),
      _syncWatch = lib.lookupFunction<_ArgRetC, _ArgRetDart>('pb_sync_watch'),
      _syncUnwatch = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_sync_unwatch',
      ),
      _syncWatches = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_sync_watches',
      ),
      _presence = lib.lookupFunction<_RetC, _RetDart>('pb_presence_json'),
      _presenceBattery = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_presence_battery',
      ),
      _clipboardSync = lib.lookupFunction<_ArgRetC, _ArgRetDart>(
        'pb_clipboard_sync',
      );

  /// Load the native library. `overridePath` forces a specific file (tests).
  static Bindings load({String? overridePath}) {
    try {
      final lib = openPeerbeamLibrary(overridePath);
      return Bindings._(lib);
    } on NativeLoadError {
      rethrow;
    } catch (e) {
      throw NativeLoadError('failed to load peerbeam-ffi: $e');
    }
  }

  int abiVersion() => _abiVersion();
  String versionJson() => _consume(_versionJson());

  /// Blocking: the engine makes one HTTPS request. Callers keep it off the UI
  /// path, which is why it only runs when a person presses the button.
  String checkUpdates() => _consume(_checkUpdates());
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
  String acceptTrust(String json) => _withArg(json, _acceptTrust);
  String reject(String json) => _withArg(json, _reject);
  String active() => _consume(_active());
  String interrupted() => _consume(_interrupted());
  String resumeInterrupted(String json) => _withArg(json, _resumeInterrupted);
  String discardInterrupted(String json) => _withArg(json, _discardInterrupted);
  String get(String json) => _withArg(json, _get);
  String history() => _consume(_history());
  String trustList() => _consume(_trustList());
  String trustApprove(String json) => _withArg(json, _trustApprove);
  String trustRemove(String json) => _withArg(json, _trustRemove);
  String trustSetPermission(String json) => _withArg(json, _trustSetPermission);
  String trustSetAutoAccept(String json) => _withArg(json, _trustSetAutoAccept);
  String rulesSet(String json) => _withArg(json, _rulesSet);
  String historyClear() => _consume(_historyClear());
  String settingsGet() => _consume(_settingsGet());
  String settingsSet(String json) => _withArg(json, _settingsSet);
  String chatSend(String json) => _withArg(json, _chatSend);
  String chatSendFile(String json) => _withArg(json, _chatSendFile);
  String chatHistory(String json) => _withArg(json, _chatHistory);
  String chatReconcile(String json) => _withArg(json, _chatReconcile);
  String chatCancel(String json) => _withArg(json, _chatCancel);
  String chatConversations(String json) => _withArg(json, _chatConversations);
  String chatDelete(String json) => _withArg(json, _chatDelete);
  String chatDeleteMessages(String json) => _withArg(json, _chatDeleteMessages);
  String chatSearch(String json) => _withArg(json, _chatSearch);
  String chatReact(String json) => _withArg(json, _chatReact);
  String chatMarkRead(String json) => _withArg(json, _chatMarkRead);
  String notesList(String json) => _withArg(json, _notesList);
  String groupsList(String json) => _withArg(json, _groupsList);
  String groupsCreate(String json) => _withArg(json, _groupsCreate);
  String groupsRename(String json) => _withArg(json, _groupsRename);
  String groupsDecline(String json) => _withArg(json, _groupsDecline);
  String groupsInvite(String json) => _withArg(json, _groupsInvite);
  String groupsAccept(String json) => _withArg(json, _groupsAccept);
  String groupsLeave(String json) => _withArg(json, _groupsLeave);
  String groupsSend(String json) => _withArg(json, _groupsSend);
  String groupsHistory(String json) => _withArg(json, _groupsHistory);
  String spacesList(String json) => _withArg(json, _spacesList);
  String spacesCreate(String json) => _withArg(json, _spacesCreate);
  String spacesRename(String json) => _withArg(json, _spacesRename);
  String spacesDelete(String json) => _withArg(json, _spacesDelete);
  String spacesAddMember(String json) => _withArg(json, _spacesAddMember);
  String spacesRemoveMember(String json) => _withArg(json, _spacesRemoveMember);
  String trustSetMine(String json) => _withArg(json, _trustSetMine);
  String trustMyDevices(String json) => _withArg(json, _trustMyDevices);
  String wakeSet(String json) => _withArg(json, _wakeSet);
  String wakeGet(String json) => _withArg(json, _wakeGet);
  String wakeForget(String json) => _withArg(json, _wakeForget);
  String wakeSend(String json) => _withArg(json, _wakeSend);
  String chatRetentionGet(String json) => _withArg(json, _chatRetentionGet);
  String chatRetentionSet(String json) => _withArg(json, _chatRetentionSet);
  String chatPrune(String json) => _withArg(json, _chatPrune);
  String notesCreate(String json) => _withArg(json, _notesCreate);
  String notesEdit(String json) => _withArg(json, _notesEdit);
  String notesDelete(String json) => _withArg(json, _notesDelete);
  String notesSync(String json) => _withArg(json, _notesSync);
  String presenceRing(String json) => _withArg(json, _presenceRing);
  String clipHistory(String json) => _withArg(json, _clipHistory);
  String clipHistoryClear(String json) => _withArg(json, _clipHistoryClear);
  String timeline(String json) => _withArg(json, _timeline);
  String browseList(String json) => _withArg(json, _browseList);
  String browseShares(String json) => _withArg(json, _browseShares);
  String logsGet(String json) => _withArg(json, _logsGet);
  String logsExport(String json) => _withArg(json, _logsExport);
  String logsSubscribe(String json) => _withArg(json, _logsSubscribe);
  String syncPull(String json) => _withArg(json, _syncPull);
  String syncWatch(String json) => _withArg(json, _syncWatch);
  String syncUnwatch(String json) => _withArg(json, _syncUnwatch);
  String syncWatches(String json) => _withArg(json, _syncWatches);

  String presence() => _consume(_presence());
  String presenceBattery(String json) => _withArg(json, _presenceBattery);

  String clipboardSync(String json) => _withArg(json, _clipboardSync);

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
///
/// Public because `off_isolate.dart` opens the same image from a background
/// isolate, and it must resolve the identical file: a second copy of this
/// logic that drifted would have one isolate talking to a different build of
/// the engine than the other.
DynamicLibrary openPeerbeamLibrary(String? overridePath) {
  if (overridePath != null) return DynamicLibrary.open(overridePath);
  if (Platform.isIOS) return DynamicLibrary.process();
  if (Platform.isMacOS) {
    // macOS `dlopen` of a bare leaf name does NOT search the app bundle's
    // Frameworks dir (unlike Linux, whose loader searches the executable's
    // RUNPATH), so resolve the embedded engine explicitly relative to the app
    // binary: peerbeam.app/Contents/MacOS/peerbeam ->
    // ../Frameworks/libpeerbeam_ffi.dylib. Fall back to the bare name for
    // `flutter test`/dev where the dylib sits on the default search path.
    final exeDir = File(Platform.resolvedExecutable).parent.path;
    final bundled = '$exeDir/../Frameworks/libpeerbeam_ffi.dylib';
    if (File(bundled).existsSync()) return DynamicLibrary.open(bundled);
    return DynamicLibrary.open('libpeerbeam_ffi.dylib');
  }
  final name = Platform.isWindows ? 'peerbeam_ffi.dll' : 'libpeerbeam_ffi.so';
  return DynamicLibrary.open(name);
}

/// Decode a JSON string into a map (utility used by the SDK).
Map<String, dynamic> decodeJson(String s) =>
    jsonDecode(s) as Map<String, dynamic>;
