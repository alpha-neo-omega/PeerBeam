import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../sdk/models.dart';
import '../../state/app_scope.dart';
import '../../widgets/common.dart';

/// What another device shares, read-only.
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

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addPostFrameCallback((_) => _load(''));
  }

  Future<void> _load(String path) async {
    setState(() {
      _loading = true;
      _path = path;
    });
    final api = AppScope.of(context).api;
    final listing = api == null
        ? null
        : await api.browse(widget.peer, path: path);
    if (!mounted) return;
    setState(() {
      _listing = listing;
      _loading = false;
    });
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
          ? const SizedBox.shrink()
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
                  onTap: e.isDir
                      ? () => _load(_path.isEmpty ? e.name : '$_path/${e.name}')
                      : null,
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
