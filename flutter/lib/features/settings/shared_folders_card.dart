import 'package:file_selector/file_selector.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../sdk/error_text.dart';
import '../../sdk/models.dart' show SharedFolder;
import '../../state/app_scope.dart';

bool get _isAndroid =>
    !kIsWeb && defaultTargetPlatform == TargetPlatform.android;

/// The shared-folders editor: what this device offers to peers who may browse.
///
/// # Three things this section has to keep saying
///
/// 1. **Nothing is shared until someone chooses it.** Empty is the default and
///    the safe state, so it says "Nothing shared" in as many words. A blank
///    card looks identical to a load that failed, and only one of those should
///    leave anyone reassured.
/// 2. **A folder that has gone is still listed.** The engine reports `exists`
///    per share; hiding the false ones would leave someone believing they
///    share something they do not, which is the wrong belief this section
///    exists to prevent. It is marked broken instead.
/// 3. **The path, not just the name.** Two folders called `Documents` are
///    indistinguishable by name, and nobody should confirm — or un-share — a
///    folder they cannot identify.
///
/// Every write is followed by a re-read rather than adopting the new list
/// locally, for the reason `_SaveRulesCard` does the same: `exists` is the
/// engine's answer, not this screen's guess, and a refused write must leave the
/// user looking at what is actually shared.
///
/// On Android the whole card is replaced by a plain statement of why there is
/// no picker, the way `_SaveRulesCard` does for rules. A picker that led
/// nowhere is worse than no picker: it spends a user's attention and leaves
/// them believing a folder is on offer.
class SharedFoldersCard extends StatefulWidget {
  const SharedFoldersCard({super.key});

  @override
  State<SharedFoldersCard> createState() => _SharedFoldersCardState();
}

class _SharedFoldersCardState extends State<SharedFoldersCard> {
  List<SharedFolder> _folders = const [];
  bool _loaded = false;

  /// The last read's failure, or null.
  Object? _error;

  @override
  void initState() {
    super.initState();
    // Nothing to read on Android: `build` never renders a list there, and
    // asking the engine for one only to throw the answer away is a call made
    // for nobody.
    if (_isAndroid) return;
    WidgetsBinding.instance.addPostFrameCallback((_) => _load());
  }

  Future<void> _load() async {
    final api = AppScope.of(context).api;
    try {
      final folders = api == null
          ? <SharedFolder>[]
          : await api.sharedFolders();
      if (!mounted) return;
      setState(() {
        _folders = folders;
        _error = null;
        _loaded = true;
      });
    } catch (e) {
      // A read that failed must not render as "Nothing shared". That sentence
      // is a claim about the engine's list, and stating it when no list came
      // back tells someone they share nothing when they may share plenty.
      if (!mounted) return;
      setState(() {
        _error = e;
        _loaded = true;
      });
    }
  }

  /// Pick a folder with the native directory chooser — the same one the save
  /// location and the save rules use. Never a text field: a share is an
  /// absolute path that has to exist, and typing one is how you get a share
  /// that silently offers nothing.
  Future<void> _add() async {
    final picked = await getDirectoryPath(confirmButtonText: 'Share');
    if (picked == null || picked.isEmpty || !mounted) return;
    if (_folders.any((f) => f.path == picked)) {
      // Re-writing it would either duplicate the row or do nothing at all;
      // either way the user is owed the reason nothing changed.
      _say('Already shared');
      return;
    }
    await _write([for (final f in _folders) f.path, picked]);
  }

  /// Stop sharing one folder, with an undo.
  ///
  /// An undo rather than a prompt, and deliberately not the other way round:
  /// un-sharing is the *safe* direction here — the belief this card exists to
  /// prevent is that a folder is private when it is still offered — so a dialog
  /// in front of it would slow the one action that must never hesitate. What is
  /// lost is a path, still legible on screen as the row goes, and putting it
  /// back is a single write.
  Future<void> _remove(SharedFolder folder) async {
    final index = _folders.indexWhere((f) => f.path == folder.path);
    // Only offer to take back what actually happened — a refused write has
    // already reported itself, and an Undo beside it would be undoing nothing.
    final removed = await _write([
      for (final f in _folders)
        if (f.path != folder.path) f.path,
    ]);
    if (!removed || !mounted) return;
    // The name, not the path: the path is what the undo would restore, and
    // repeating it in a snackbar makes it read as though it were still shared.
    _say(
      'Stopped sharing ${folder.name.isEmpty ? folder.path : folder.name}',
      action: SnackBarAction(
        label: 'Undo',
        onPressed: () => _restore(folder, index),
      ),
    );
  }

  /// Put a share back where it was.
  ///
  /// Composed against the list as it stands now rather than a snapshot taken at
  /// removal: two removals in a row would otherwise have the first undo
  /// resurrect the second folder as well. Already-present is a no-op, since a
  /// re-shared folder written twice would either duplicate the row or fail.
  Future<void> _restore(SharedFolder folder, int index) async {
    if (!mounted || _folders.any((f) => f.path == folder.path)) return;
    final paths = [for (final f in _folders) f.path];
    paths.insert(index.clamp(0, paths.length), folder.path);
    await _write(paths);
  }

  /// Persist, surface a refusal, then re-read whatever is actually in force.
  /// Returns whether the engine took the list — the undo offered after a
  /// removal must not appear beside a write that was refused.
  Future<bool> _write(List<String> paths) async {
    final api = AppScope.of(context).api;
    if (api == null) return false;
    var written = true;
    try {
      await api.setSharedFolders(paths);
    } catch (e) {
      written = false;
      if (mounted) _say(friendlyError(e));
    }
    await _load();
    return written;
  }

  void _say(String message, {SnackBarAction? action}) {
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(message), action: action));
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    if (_isAndroid) return _unsupported();
    return Card(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(
              AppSpace.md,
              AppSpace.md,
              AppSpace.md,
              0,
            ),
            child: Text(
              // Load-bearing copy: who can see these, and — because there is no
              // Save button anywhere on this card — that a removal has already
              // happened by the time the row disappears.
              'Devices you have granted Browse can list what is in these '
              'folders and copy files out of them. Adding and removing take '
              'effect immediately, not at the next restart.',
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
            ),
          ),
          ..._rows(theme),
          const Divider(height: 1),
          Align(
            alignment: Alignment.centerLeft,
            child: Padding(
              padding: const EdgeInsets.all(AppSpace.sm),
              child: TextButton.icon(
                onPressed: _add,
                icon: const Icon(Icons.add_rounded),
                label: const Text('Share a folder'),
              ),
            ),
          ),
        ],
      ),
    );
  }

  /// The rows, or the empty state. Nothing at all until the first read has
  /// answered — "Nothing shared" is a claim about the engine's list, and
  /// showing it before there is one would state it of a list nobody has read.
  List<Widget> _rows(ThemeData theme) {
    if (!_loaded) {
      return const [
        ListTile(
          leading: SizedBox.square(
            dimension: 24,
            child: Center(
              child: SizedBox.square(
                dimension: 16,
                child: CircularProgressIndicator(strokeWidth: 2),
              ),
            ),
          ),
          title: Text('Reading shared folders…'),
        ),
      ];
    }
    if (_error != null) {
      return [
        ListTile(
          leading: Icon(
            Icons.error_outline_rounded,
            color: theme.colorScheme.error,
          ),
          title: const Text('Could not read shared folders'),
          subtitle: Text(friendlyError(_error!)),
          trailing: IconButton(
            icon: const Icon(Icons.refresh_rounded),
            tooltip: 'Try again',
            onPressed: _load,
          ),
        ),
      ];
    }
    if (_folders.isEmpty) {
      return const [
        ListTile(
          leading: Icon(Icons.folder_off_outlined),
          title: Text('Nothing shared'),
          subtitle: Text(
            'PeerBeam shares no folder until you choose one. This is the '
            'default, not a problem.',
          ),
        ),
      ];
    }
    return [
      for (var i = 0; i < _folders.length; i++) ...[
        if (i > 0) const Divider(height: 1),
        _tile(_folders[i], theme),
      ],
    ];
  }

  Widget _tile(SharedFolder folder, ThemeData theme) => ListTile(
    // Keyed by path. Names are now assigned uniquely by the engine — a second
    // `Documents` becomes `Documents (2)` — but the path is still the field
    // that cannot collide, and it is still shown, because two folders with
    // distinct names can be indistinguishable to the person who chose them.
    key: Key('shared-folder-${folder.path}'),
    leading: Icon(
      folder.exists
          ? Icons.folder_shared_rounded
          : Icons.report_problem_rounded,
      color: folder.exists ? null : theme.colorScheme.error,
    ),
    // The fallback stays: an empty name meant an unaddressable root before the
    // engine started assigning them, and a row that renders blank is worse than
    // one that renders a path.
    title: Text(folder.name.isEmpty ? folder.path : folder.name),
    subtitle: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(folder.path),
        if (!folder.exists)
          Text(
            'This folder is gone — peers see nothing here.',
            style: theme.textTheme.bodySmall?.copyWith(
              color: theme.colorScheme.error,
            ),
          ),
      ],
    ),
    isThreeLine: !folder.exists,
    trailing: IconButton(
      tooltip: 'Stop sharing',
      icon: const Icon(Icons.remove_circle_outline_rounded),
      onPressed: () => _remove(folder),
    ),
  );
}
