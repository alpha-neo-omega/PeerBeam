import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../sdk/error_text.dart';
import '../../sdk/models.dart' show SharedFolder;
import '../../state/app_scope.dart';

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

  Future<void> _remove(SharedFolder folder) => _write([
    for (final f in _folders)
      if (f.path != folder.path) f.path,
  ]);

  /// Persist, surface a refusal, then re-read whatever is actually in force.
  Future<void> _write(List<String> paths) async {
    final api = AppScope.of(context).api;
    if (api == null) return;
    try {
      await api.setSharedFolders(paths);
    } catch (e) {
      if (mounted) _say(friendlyError(e));
    }
    await _load();
  }

  void _say(String message) {
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(message)));
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
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
    // Keyed by path, the only field guaranteed to be unique — two shares can
    // and do carry the same name, which is the whole reason the path is shown.
    key: Key('shared-folder-${folder.path}'),
    leading: Icon(
      folder.exists
          ? Icons.folder_shared_rounded
          : Icons.report_problem_rounded,
      color: folder.exists ? null : theme.colorScheme.error,
    ),
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
