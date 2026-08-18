import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../sdk/models.dart';
import '../../state/app_scope.dart';
import '../../widgets/appear.dart';
import '../../widgets/common.dart';

/// One chronological view of what this device has done.
///
/// Deliberately thin: every entry says *that* something happened, to whom, and
/// when. It carries no message bodies and no clip text, because the screens
/// built for reading those do it far better — and an activity feed full of
/// content is a second place every secret lives.
class TimelineScreen extends StatefulWidget {
  const TimelineScreen({super.key});

  @override
  State<TimelineScreen> createState() => _TimelineScreenState();
}

class _TimelineScreenState extends State<TimelineScreen> {
  List<TimelineEvent> _events = const [];
  bool _loaded = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addPostFrameCallback((_) => _load());
  }

  Future<void> _load() async {
    final api = AppScope.of(context).api;
    final events = api == null
        ? <TimelineEvent>[]
        : await api.timeline(limit: 200);
    if (!mounted) return;
    setState(() {
      _events = events;
      _loaded = true;
    });
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('Activity'),
        actions: [
          IconButton(
            icon: const Icon(Icons.refresh_rounded),
            tooltip: 'Refresh',
            onPressed: _load,
          ),
        ],
      ),
      body: !_loaded
          ? const SizedBox.shrink()
          : _events.isEmpty
          ? const EmptyState(
              icon: Icons.timeline_rounded,
              title: 'Nothing yet',
              message: 'Transfers, messages and clips will appear here.',
            )
          : ListView.builder(
              padding: const EdgeInsets.all(AppSpace.md),
              itemCount: _events.length,
              itemBuilder: (context, i) {
                final e = _events[i];
                return Appear(
                  index: i,
                  child: ListTile(
                    leading: Icon(_icon(e), color: _color(context, e)),
                    title: Text(_title(e)),
                    subtitle: Text(_when(e.at)),
                  ),
                );
              },
            ),
    );
  }

  static IconData _icon(TimelineEvent e) => switch (e.kind) {
    'transfer' => Icons.swap_horiz_rounded,
    'chat' => Icons.forum_outlined,
    'clipboard' => Icons.content_paste_rounded,
    _ => Icons.circle_outlined,
  };

  static Color? _color(BuildContext context, TimelineEvent e) =>
      e.kind == 'transfer' && !e.ok
      ? Theme.of(context).colorScheme.error
      : null;

  /// What the row says. Names the device where there is one, because "a
  /// message" tells the reader nothing they did not already know.
  static String _title(TimelineEvent e) {
    final who = e.peer.isEmpty ? 'this device' : e.peer;
    return switch (e.kind) {
      'transfer' =>
        e.detail.isEmpty
            ? (e.ok ? 'Transfer with $who' : 'Transfer with $who failed')
            : (e.ok ? '${e.detail} — $who' : '${e.detail} — $who (failed)'),
      'chat' =>
        e.detail.isEmpty ? 'Message with $who' : 'Shared ${e.detail} with $who',
      'clipboard' =>
        e.peer.isEmpty ? 'Clipboard copied here' : 'Clipboard from $who',
      _ => who,
    };
  }

  static String _when(DateTime at) {
    final d = DateTime.now().difference(at);
    if (d.inMinutes < 1) return 'just now';
    if (d.inHours < 1) return '${d.inMinutes}m ago';
    if (d.inDays < 1) return '${d.inHours}h ago';
    return '${d.inDays}d ago';
  }
}
