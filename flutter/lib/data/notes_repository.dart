import 'package:flutter/foundation.dart';

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

  /// Create a note and return its id, or null when there is no engine.
  Future<String?> create(String body, {String title = ''}) async {
    final api = _api;
    if (api == null) return null;
    try {
      final id = await api.notesCreate(body, title: title);
      await refresh();
      return id;
    } catch (_) {
      return null;
    }
  }

  /// Replace a note's content. Returns whether anything changed — `false` also
  /// means the note was deleted elsewhere, which the caller should surface
  /// rather than swallow: the user's edit did not land.
  Future<bool> edit(String id, String body, {String title = ''}) async {
    final api = _api;
    if (api == null) return false;
    try {
      final ok = await api.notesEdit(id, body, title: title);
      await refresh();
      return ok;
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
