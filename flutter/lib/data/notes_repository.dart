import 'package:flutter/foundation.dart';

import '../sdk/error_text.dart';

import '../sdk/models.dart';
import '../sdk/peerbeam.dart';

/// Notes, read from and written through the engine.
///
/// Holds no note of its own beyond the last list it was given: the engine owns
/// storage, conflict resolution and tombstones, and a second copy here would be
/// one more thing that can disagree with it.
class NotesRepository extends ChangeNotifier {
  final PeerBeamApi? _api;

  /// Named `api` for symmetry with every other repository's constructor, while
  /// the field stays private.
  NotesRepository({PeerBeamApi? api})
    // ignore: prefer_initializing_formals
    : _api = api;

  List<Note> notes = const [];

  /// True once a load has completed, so an empty list can be told apart from
  /// "not loaded yet" — otherwise the empty state flashes on every open.
  bool loaded = false;

  Future<void> refresh() async {
    final api = _api;
    if (api == null) {
      loaded = true;
      notifyListeners();
      return;
    }
    try {
      notes = await api.notesList();
    } catch (_) {
      // A failed read leaves the previous list standing rather than blanking
      // the screen: stale notes are more useful than none, and the next
      // refresh corrects them.
    }
    loaded = true;
    notifyListeners();
  }

  /// Create a note. Returns null when it was written, or a message when it was
  /// not.
  ///
  /// **The failure has to reach the caller.** This used to answer a nullable id
  /// and fold every refusal into null, and the screen did not read even that —
  /// so a note the engine would not write vanished the moment the dialog closed,
  /// with the text gone and nothing said. Losing something a person just typed
  /// is the worst thing this screen can do.
  Future<String?> create(String body, {String title = ''}) async {
    final api = _api;
    if (api == null) return 'PeerBeam is not running.';
    try {
      await api.notesCreate(body, title: title);
      await refresh();
      return null;
    } catch (e) {
      return friendlyError(e);
    }
  }

  /// Replace a note's content. Returns null when it was saved, or the reason it
  /// was not.
  ///
  /// **Two different failures, told apart.** The engine answering `false` means
  /// the note is gone — deleted elsewhere — while an exception means the write
  /// failed for some other reason. This used to return `false` for both, and the
  /// screen reported every one of them as "That note was deleted": a message
  /// that is a guess, and one that tells the user to stop looking for text that
  /// may still be there.
  Future<String?> edit(String id, String body, {String title = ''}) async {
    final api = _api;
    if (api == null) return 'PeerBeam is not running.';
    try {
      final ok = await api.notesEdit(id, body, title: title);
      await refresh();
      return ok ? null : 'That note was deleted, so the edit was not saved.';
    } catch (e) {
      return friendlyError(e);
    }
  }

  /// Exchange notes with [peer].
  ///
  /// The peer's own set arrives asynchronously through the session, so this
  /// refreshes afterwards: what came back is already in the store by the time
  /// a user could act on the result, and re-reading is cheaper than inventing
  /// an event for it.
  Future<bool> sync(PeerTarget peer) async {
    final api = _api;
    if (api == null) return false;
    try {
      final sent = await api.notesSync(peer);
      await refresh();
      return sent;
    } catch (_) {
      return false;
    }
  }

  Future<bool> delete(String id) async {
    final api = _api;
    if (api == null) return false;
    try {
      final ok = await api.notesDelete(id);
      await refresh();
      return ok;
    } catch (_) {
      return false;
    }
  }
}
