// ignore_for_file: prefer_initializing_formals
import 'dart:async';

import 'package:flutter/services.dart';

import '../platform/desktop_files.dart' show isDesktop;
import '../sdk/events.dart';
import '../sdk/exceptions.dart';
import '../sdk/models.dart';
import '../sdk/peerbeam.dart';

/// The desktop clipboard watcher, and the echo guard that keeps two devices
/// from syncing the same clip back and forth forever.
///
/// **Why the watcher lives here and not in Rust.** Watching a clipboard means
/// reading one, and there is no system-clipboard adapter in the Rust workspace
/// — only an in-memory double. Flutter's [Clipboard] already works on every
/// desktop target, so auto-sync is a GUI feature and the CLI gains nothing from
/// it (`docs/CLI.md`).
///
/// **Desktop sends, every platform receives.** Android 10+ forbids reading the
/// clipboard from the background, so a phone can never auto-send: no permission
/// changes that, and a watcher that ran there would poll forever and find
/// nothing it is allowed to see. [ClipboardSyncService.start] therefore refuses
/// off desktop. Receiving is unaffected — a phone applies an incoming clip in
/// full, which is why the engine advertises the capability from every platform.
///
/// **Everything copied while this is on is sent, passwords included.** There is
/// no password detection anywhere in this feature and there is not meant to be:
/// [Clipboard.getData] returns plain text with no sensitivity signal, and
/// X11/Wayland define none, so a heuristic would be wrong in both directions —
/// silently dropping clips the user expected, or shipping a credential while
/// the UI implies something was checked. The honest answer is the warning in
/// Settings, which `test/clipboard_sync_test.dart` pins so it cannot be quietly
/// tidied away.

/// The one decision: *is what the clipboard now reads a new local copy that
/// should be pushed?*
///
/// Kept as its own object, pure and free of timers and IO, for the same reason
/// `peerbeam_clipboard::may_share_clip` is one function: there is exactly one
/// place to read, one place to test, and no way for a refactor to lose it
/// quietly.
class ClipboardEchoGuard {
  String? _accounted;

  /// The last clipboard content this device has already dealt with — either
  /// because it sent it, or because a peer sent it to us.
  String? get accounted => _accounted;

  /// Record [text] as dealt with.
  ///
  /// Called from **both** sides, and the receiving side is the load-bearing
  /// one. Without it, a clip Bob sends is written to our clipboard, the next
  /// poll sees "new content", and we send it straight back to Bob — whose
  /// watcher then sees *his* clipboard change and returns it. Two devices
  /// ping-pong a single copy forever, over the network, at one round trip per
  /// second each.
  void adopt(String text) => _accounted = text;

  /// Whether [current] should be pushed to peers.
  ///
  /// False for the empty clipboard (nothing to sync, and an empty clip would
  /// *erase* every peer's clipboard — the engine refuses one for that reason)
  /// and false for content already accounted for, which covers both "the user
  /// has not copied anything since" and "this is the clip a peer just sent us".
  bool shouldSend(String current) =>
      current.isNotEmpty && current != _accounted;
}

/// A clip a peer applied to this device's clipboard.
typedef AppliedClip = ({String deviceId, String text});

/// Polls the system clipboard while the opt-in is on, pushes what changed to
/// trusted peers, and applies what they push back.
class ClipboardSyncService {
  final PeerBeamApi? _api;
  final List<PeerTarget> Function() _peers;
  final String Function(String deviceId) _nameOf;
  final Future<String?> Function() _read;
  final Future<void> Function(String) _write;
  final Duration _interval;
  final bool _desktop;

  final ClipboardEchoGuard _guard = ClipboardEchoGuard();
  final StreamController<String> _notices = StreamController<String>.broadcast();
  final StreamController<AppliedClip> _applied =
      StreamController<AppliedClip>.broadcast();
  StreamSubscription<BridgeEvent>? _sub;
  Timer? _timer;
  bool _pushing = false;
  bool _disposed = false;

  ClipboardSyncService({
    PeerBeamApi? api,
    required List<PeerTarget> Function() peers,
    String Function(String deviceId)? nameOf,
    Future<String?> Function()? readClipboard,
    Future<void> Function(String)? writeClipboard,
    Duration interval = const Duration(seconds: 1),
    bool? desktop,
  }) : _api = api,
       _peers = peers,
       _nameOf = nameOf ?? _defaultName,
       _read = readClipboard ?? _readSystemClipboard,
       _write = writeClipboard ?? _writeSystemClipboard,
       _interval = interval,
       _desktop = desktop ?? isDesktop {
    // Receiving is subscribed unconditionally — never gated on the opt-in and
    // never gated on being desktop. The setting governs what *leaves* this
    // machine; a device that shares nothing still applies what its trusted
    // peers send, and on Android that is the only half that can ever run.
    _sub = _api?.events.listen((e) {
      if (e is ClipboardReceived) unawaited(_apply(e));
    });
  }

  /// Messages the user should see: a clip applied, or one too large to sync.
  Stream<String> get notices => _notices.stream;

  /// Clips applied to this device's clipboard, for anything that wants the
  /// content rather than the message.
  Stream<AppliedClip> get applied => _applied.stream;

  /// Whether the poll loop is running.
  bool get watching => _timer != null;

  /// The echo guard, exposed so tests can see what has been accounted for.
  ClipboardEchoGuard get guard => _guard;

  /// Start watching. **No-ops off desktop** and is idempotent.
  ///
  /// The current clipboard is adopted without being sent: whatever was already
  /// on the clipboard when the user flipped the toggle is not "something they
  /// just copied", and pushing it would sync a buffer they had forgotten about
  /// — quite possibly the password they pasted five minutes ago. Sync starts
  /// with the *next* copy.
  void start() {
    if (_disposed || !_desktop || _timer != null) return;
    unawaited(_read().then((current) {
      if (current != null && current.isNotEmpty) _guard.adopt(current);
    }));
    _timer = Timer.periodic(_interval, (_) => unawaited(poll()));
  }

  /// Stop watching. Idempotent, and safe to call when never started.
  ///
  /// The guard keeps its state on purpose: turning sync off and on again must
  /// not re-push whatever happens to be on the clipboard at that moment.
  void stop() {
    _timer?.cancel();
    _timer = null;
  }

  /// Follow the opt-in setting. Called whenever it changes, so turning sync on
  /// or off takes effect immediately rather than at the next app start.
  void applySetting({required bool enabled}) {
    if (enabled) {
      start();
    } else {
      stop();
    }
  }

  /// One poll: read the clipboard, and push it if it is genuinely new.
  ///
  /// Exposed (rather than private) so tests can drive ticks deterministically
  /// instead of sleeping on a real timer.
  Future<void> poll() async {
    if (_disposed || _pushing) return;
    final api = _api;
    if (api == null) return;
    String? current;
    try {
      current = await _read();
    } catch (_) {
      return; // a clipboard we cannot read is not a clipboard that changed
    }
    if (current == null || !_guard.shouldSend(current)) return;

    // Adopt **before** awaiting the push. The clip is accounted for the moment
    // we decide to send it: a slow push must not let the next tick see the
    // same content as new and send it twice.
    _guard.adopt(current);
    final peers = _peers();
    if (peers.isEmpty) return;

    _pushing = true;
    try {
      await api.clipboardSync(current, peers);
    } on PeerBeamException catch (e) {
      // The engine refused it — over the cap, most likely. Say so once. The
      // content stays adopted, so this does not repeat every second for a clip
      // that can never be sent.
      _notice(_refusalText(e));
    } catch (_) {
      // A transient engine/transport failure. The clipboard is live state, so
      // nothing is queued and nothing is retried: delivering what was copied
      // ten minutes ago on top of what has been copied since would be worse
      // than not delivering it.
    } finally {
      _pushing = false;
    }
  }

  /// A peer's clip arrived: account for it, apply it, then say so.
  ///
  /// **The order is the echo guard.** [ClipboardEchoGuard.adopt] runs before
  /// the clipboard is written, so a poll landing between the two still sees
  /// content it has already accounted for. Writing first and adopting after
  /// leaves exactly the window in which this device sends the clip back to the
  /// peer that just sent it.
  Future<void> _apply(ClipboardReceived e) async {
    if (_disposed || e.text.isEmpty) return;
    _guard.adopt(e.text);
    try {
      await _write(e.text);
    } catch (_) {
      return; // could not write it, so do not claim we did
    }
    if (_disposed) return;
    _applied.add((deviceId: e.deviceId, text: e.text));
    // The clipboard changed under the user, so they are told who changed it.
    // Never the content: it is on their clipboard already, and a toast is a
    // poor place for something that may be a password.
    _notice('Clipboard from ${_nameOf(e.deviceId)}');
  }

  void _notice(String message) {
    if (!_disposed && _notices.hasListener) _notices.add(message);
  }

  /// The message for a refused push. An over-cap clip is the case worth
  /// naming: the user copied something and it did not arrive, and silence
  /// would read as a broken feature rather than a deliberate limit. The
  /// engine's message already states the size and the limit.
  static String _refusalText(PeerBeamException e) => switch (e) {
    InvalidArgumentException() => 'Clipboard not synced: ${e.message}',
    _ => 'Clipboard not synced',
  };

  void dispose() {
    _disposed = true;
    stop();
    _sub?.cancel();
    _notices.close();
    _applied.close();
  }

  static Future<String?> _readSystemClipboard() async =>
      (await Clipboard.getData(Clipboard.kTextPlain))?.text;

  static Future<void> _writeSystemClipboard(String text) =>
      Clipboard.setData(ClipboardData(text: text));

  /// Falls back to the id when no name is known — an unnamed device is still
  /// better identified than "a device".
  static String _defaultName(String deviceId) => deviceId;
}
