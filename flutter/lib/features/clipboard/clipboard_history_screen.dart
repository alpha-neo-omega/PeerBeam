import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../app/theme.dart';
import '../../sdk/models.dart';
import '../../state/app_scope.dart';
import '../../widgets/common.dart';

/// What this device remembers having copied.
///
/// Entries are shown **abbreviated**, and the full text only when the user asks
/// by copying it back. A screen that rendered fifty clips in full would put
/// every remembered password on one page — which is the thing bounding the log
/// was meant to avoid.
class ClipboardHistoryScreen extends StatefulWidget {
  const ClipboardHistoryScreen({super.key});

  @override
  State<ClipboardHistoryScreen> createState() => _ClipboardHistoryScreenState();
}

class _ClipboardHistoryScreenState extends State<ClipboardHistoryScreen> {
  List<ClipEntry> _entries = const [];
  bool _loaded = false;

  /// The last read's failure, or null. An absence and a failure must not
  /// render the same: one is a fact about the world, the other about us.
  Object? _error;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addPostFrameCallback((_) => _load());
  }

  Future<void> _load() async {
    final api = AppScope.of(context).api;
    try {
      final entries = api == null
          ? <ClipEntry>[]
          : await api.clipboardHistory();
      if (!mounted) return;
      setState(() {
        _entries = entries;
        _error = null;
        _loaded = true;
      });
    } catch (e) {
      if (!mounted) return;
      setState(() {
        _error = e;
        _loaded = true;
      });
    }
  }

  Future<void> _clear() async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (dialogContext) => AlertDialog(
        title: const Text('Erase clipboard history?'),
        content: const Text(
          'Everything this device remembers copying will be deleted. This '
          'cannot be undone.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(dialogContext).pop(false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(dialogContext).pop(true),
            child: const Text('Erase'),
          ),
        ],
      ),
    );
    if (confirmed != true || !mounted) return;
    final api = AppScope.of(context).api;
    if (api != null) await api.clipboardHistoryClear();
    await _load();
  }

  @override
  Widget build(BuildContext context) {
    final settings = AppScope.of(context).settings;
    return Scaffold(
      appBar: AppBar(
        title: const Text('Clipboard history'),
        actions: [
          if (_entries.isNotEmpty)
            IconButton(
              icon: const Icon(Icons.delete_sweep_outlined),
              tooltip: 'Erase everything',
              onPressed: _clear,
            ),
        ],
      ),
      // Subscribed to settings, not merely read: the empty state says whether
      // history is off, and that sentence has to change the moment the setting
      // does rather than at the next push.
      body: AnimatedBuilder(
        animation: settings,
        builder: (context, _) => !_loaded
            ? const Center(child: CircularProgressIndicator())
            : _error != null
            ? ErrorState(
                error: _error!,
                title: 'Could not read clipboard history',
                onRetry: _load,
              )
            : _entries.isEmpty
            // "Off" and "nothing yet" are different facts, and a user staring at
            // an empty screen deserves to know which one they are looking at.
            ? EmptyState(
                icon: Icons.content_paste_off_rounded,
                title: settings.clipboardHistory
                    ? 'Nothing remembered yet'
                    : 'Clipboard history is off',
                message: settings.clipboardHistory
                    ? 'Clips you copy or receive will appear here.'
                    : 'Turn it on in Settings to start remembering clips.',
              )
            : ListView.builder(
                padding: const EdgeInsets.all(AppSpace.md),
                itemCount: _entries.length,
                itemBuilder: (context, i) {
                  final e = _entries[i];
                  return Card(
                    child: ListTile(
                      title: Text(
                        _preview(e.text),
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                      ),
                      subtitle: Text(
                        e.isMine ? 'Copied here' : 'From ${e.from}',
                      ),
                      trailing: IconButton(
                        icon: const Icon(Icons.copy_rounded),
                        tooltip: 'Copy',
                        onPressed: () async {
                          await Clipboard.setData(ClipboardData(text: e.text));
                          if (!context.mounted) return;
                          ScaffoldMessenger.of(context).showSnackBar(
                            const SnackBar(content: Text('Copied')),
                          );
                        },
                      ),
                    ),
                  );
                },
              ),
      ),
    );
  }

  /// A one-line preview: first line, capped. Never the whole clip — see the
  /// class doc.
  static String _preview(String text) {
    final line = text.split('\n').first.trim();
    if (line.length <= 80) return line;
    return '${line.substring(0, 79)}…';
  }
}
