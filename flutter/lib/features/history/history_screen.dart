import 'package:flutter/material.dart';

import '../../state/app_scope.dart';
import '../../state/models.dart';
import '../../widgets/appear.dart';
import '../../widgets/common.dart';

/// Completed-transfer history. Listens to the history store only; uses a
/// lazy builder so a long history never builds every row up front.
class HistoryScreen extends StatelessWidget {
  const HistoryScreen({super.key});

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    return Scaffold(
      appBar: AppBar(
        title: const Text('History'),
        actions: [
          AnimatedBuilder(
            animation: state.history,
            builder: (context, _) => IconButton(
              tooltip: 'Clear history',
              onPressed:
                  state.history.items.isEmpty ? null : state.history.clear,
              icon: const Icon(Icons.delete_sweep_rounded),
            ),
          ),
        ],
      ),
      body: SafeArea(
        child: ContentPane(
          child: AnimatedBuilder(
            animation: state.history,
            builder: (context, _) {
              final items = state.history.items;
              if (items.isEmpty) {
                return const EmptyState(
                  icon: Icons.history_rounded,
                  title: 'Nothing here yet',
                  message: 'Your completed transfers will appear here.',
                );
              }
              return ListView.builder(
                padding: const EdgeInsets.all(16),
                itemCount: items.length,
                itemBuilder: (context, i) => Appear(
                  index: i,
                  child: _HistoryRow(item: items[i]),
                ),
              );
            },
          ),
        ),
      ),
    );
  }
}

class _HistoryRow extends StatelessWidget {
  final HistoryItem item;
  const _HistoryRow({required this.item});

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final sending = item.direction == TransferDirection.sending;
    final statusColor =
        item.success ? const Color(0xFF22C55E) : scheme.error;

    return Card(
      margin: const EdgeInsets.only(bottom: 10),
      child: ListTile(
        contentPadding: const EdgeInsets.symmetric(horizontal: 14, vertical: 6),
        leading: CircleAvatar(
          backgroundColor: statusColor.withValues(alpha: 0.16),
          child: Icon(
            item.success
                ? (sending ? Icons.upload_rounded : Icons.download_rounded)
                : Icons.error_outline_rounded,
            color: statusColor,
          ),
        ),
        title: Text(
          item.fileName,
          maxLines: 1,
          overflow: TextOverflow.ellipsis,
        ),
        subtitle: Text(
          '${sending ? 'Sent to' : 'Received from'} ${item.peerName} · '
          '${formatBytes(item.bytes)} · ${_ago(item.at)}'
          '${item.success ? '' : ' · Failed'}',
        ),
        isThreeLine: false,
      ),
    );
  }

  String _ago(DateTime t) {
    final d = DateTime.now().difference(t);
    if (d.inMinutes < 1) return 'just now';
    if (d.inMinutes < 60) return '${d.inMinutes}m ago';
    if (d.inHours < 24) return '${d.inHours}h ago';
    return '${d.inDays}d ago';
  }
}
