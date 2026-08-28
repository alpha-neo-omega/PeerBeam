import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../sdk/error_text.dart';
import '../../sdk/models.dart';
import '../../state/app_scope.dart';
import '../../widgets/common.dart';

/// What another device shares, read-only — and where a folder sync is started.
///
/// Navigation is by share-relative path, because that is what the wire carries:
/// a device's real filesystem layout is not this device's business, and there
/// is nothing here that could render an absolute path even by mistake.
class BrowseScreen extends StatefulWidget {
  final PeerTarget peer;
  const BrowseScreen({super.key, required this.peer});

  @override
  State<BrowseScreen> createState() => _BrowseScreenState();
}

class _BrowseScreenState extends State<BrowseScreen> {
  /// The path being shown. Empty is the list of shares.
  String _path = '';
  BrowseListing? _listing;
  bool _loading = true;

  /// The last read's failure, or null. Distinct from an empty listing: one
  /// means the folder is empty, the other means we do not know.
  Object? _error;
  bool _syncing = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addPostFrameCallback((_) => _load(''));
  }

  /// Ask where to put it, then sync.
  ///
  /// Two-way, and the result says so: a sync that reported only "12 files" —
  /// while quietly deleting one and leaving two conflicts — would be a summary
  /// that hides the parts a person needs to act on.
  Future<void> _startSync() async {
    final api = AppScope.of(context).api;
    if (api == null) return;
    final into = await getDirectoryPath(confirmButtonText: 'Sync here');
    if (into == null || !mounted) return;

    setState(() => _syncing = true);
    String message;
    try {
      final r = await api.syncFolder(widget.peer, _path, into);
      if (r.isIdle) {
        message = 'Already in sync';
      } else {
        final parts = <String>[
          if (r.fetching > 0) '${r.fetching} in',
          if (r.pushing > 0) '${r.pushing} out',
          if (r.renamed > 0) '${r.renamed} moved',
          if (r.deleted > 0) '${r.deleted} deleted',
        ];
        message = parts.isEmpty ? 'Syncing' : 'Syncing: ${parts.join(', ')}';
        if (r.failed.isNotEmpty) {
          // Named, and said plainly. A file the peer could not give us is the
          // one thing about a sync the user has to know, and it used to be the
          // one thing they were not told.
          message +=
              " — couldn't fetch ${r.failed.length == 1 ? r.failed.single : r.failed.join(', ')}";
        }
        if (r.conflicts.isNotEmpty) {
          // Named, not counted: a conflict is a decision, and "2 conflicts"
          // tells nobody which files to look at.
          message +=
              ' — kept both copies of ${r.conflicts.length == 1 ? r.conflicts.single : r.conflicts.join(', ')}';
        }
      }
    } catch (e) {
      // Through `friendlyError`, like every other failure this app shows. The
      // raw `$e` here was an engine/FFI exception string — the one thing the
      // SDK's error layer exists to keep off screen, and useless to the person
      // reading it: "a folder that cannot be reached" is actionable, a Dart
      // exception's toString is not.
      message = 'Sync failed — ${friendlyError(e)}';
    }
    if (!mounted) return;
    setState(() => _syncing = false);
    ScaffoldMessenger.of(
      context,
    ).showSnackBar(SnackBar(content: Text(message)));
  }

  Future<void> _load(String path) async {
    setState(() {
      _loading = true;
      _error = null;
      _path = path;
    });
    final api = AppScope.of(context).api;
    try {
      final listing = api == null
          ? null
          : await api.browse(widget.peer, path: path);
      if (!mounted) return;
      setState(() {
        _listing = listing;
        _loading = false;
      });
    } catch (e) {
      // **Caught, or the screen never comes back.** This throw used to escape
      // the post-frame callback that started the load, so `_loading` stayed
      // true and the body stayed an empty box — for good. A peer that has gone
      // to sleep is the ordinary case here, not an exceptional one.
      if (!mounted) return;
      setState(() {
        _error = e;
        _loading = false;
      });
    }
  }

  void _up() {
    final parts = _path.split('/')..removeWhere((p) => p.isEmpty);
    if (parts.isEmpty) return;
    parts.removeLast();
    _load(parts.join('/'));
  }

  @override
  Widget build(BuildContext context) {
    final listing = _listing;
    return Scaffold(
      appBar: AppBar(
        title: Text(widget.peer.name),
        actions: [
          // Offered only inside a share. At the top level `_path` is empty and
          // there is no single folder to sync — syncing "everything they share"
          // would be a much larger promise than this button can make.
          if (_path.isNotEmpty)
            IconButton(
              onPressed: _syncing ? null : _startSync,
              icon: _syncing
                  ? const SizedBox(
                      width: 18,
                      height: 18,
                      child: CircularProgressIndicator(strokeWidth: 2),
                    )
                  : const Icon(Icons.sync),
              tooltip: 'Sync this folder to a local directory',
            ),
        ],
        // The path, not a filesystem location — see the class doc.
        bottom: _path.isEmpty
            ? null
            : PreferredSize(
                preferredSize: const Size.fromHeight(28),
                child: Align(
                  alignment: Alignment.centerLeft,
                  child: Padding(
                    padding: const EdgeInsets.only(
                      left: AppSpace.md,
                      bottom: AppSpace.xs,
                    ),
                    child: Text(_path),
                  ),
                ),
              ),
        leading: _path.isEmpty
            ? null
            : IconButton(
                icon: const Icon(Icons.arrow_upward_rounded),
                tooltip: 'Up',
                onPressed: _up,
              ),
      ),
      body: _loading
          ? const Center(child: CircularProgressIndicator())
          : _error != null
          ? ErrorState(
              error: _error!,
              title: 'Could not open that folder',
              onRetry: () => _load(_path),
            )
          : listing == null || listing.entries.isEmpty
          // One message for every reason, because the device sent one answer
          // for every reason. Naming a single cause would be inventing
          // information the protocol deliberately withholds.
          ? const EmptyState(
              icon: Icons.folder_off_outlined,
              title: 'Nothing to show',
              message:
                  'This device may not share anything here, may not have given '
                  'you permission to look, or the folder may be gone.',
            )
          : ListView.builder(
              itemCount: listing.entries.length,
              itemBuilder: (context, i) {
                final e = listing.entries[i];
                return ListTile(
                  leading: Icon(
                    e.isDir
                        ? Icons.folder_rounded
                        : Icons.insert_drive_file_outlined,
                  ),
                  title: Text(e.name),
                  subtitle: e.isDir ? null : Text(_size(e.size)),
                  // **A tap on a file used to do nothing at all.** The row
                  // looks exactly like the folder rows above it, and there is no
                  // per-file fetch — files arrive by syncing the folder that
                  // holds them. A dead tap leaves the user pressing harder; a
                  // sentence tells them where the action actually is, which is
                  // the button already on this screen.
                  onTap: e.isDir
                      ? () => _load(_path.isEmpty ? e.name : '$_path/${e.name}')
                      : () => ScaffoldMessenger.of(context).showSnackBar(
                          const SnackBar(
                            content: Text(
                              'Files come across with the folder. Use '
                              '"Sync here" to copy this folder to your device.',
                            ),
                          ),
                        ),
                );
              },
            ),
    );
  }

  static String _size(int bytes) {
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    var size = bytes.toDouble();
    var unit = 0;
    while (size >= 1024 && unit < units.length - 1) {
      size /= 1024;
      unit++;
    }
    return unit == 0 ? '$bytes B' : '${size.toStringAsFixed(1)} ${units[unit]}';
  }
}
