import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../state/app_scope.dart';
import '../../state/models.dart';
import '../../widgets/appear.dart';
import '../../widgets/common.dart';

/// Completed-transfer history. Listens to the history store only; uses a
/// lazy builder so a long history never builds every row up front.
class HistoryScreen extends StatelessWidget {
  const HistoryScreen({super.key});

  /// Confirm before clearing — a destructive, irreversible action.
  Future<void> _confirmClear(BuildContext context) async {
    final state = AppScope.of(context);
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('Clear history?'),
        content: const Text(
          'This removes all completed-transfer records. It cannot be undone.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(ctx, true),
            child: const Text('Clear'),
          ),
        ],
      ),
    );
    if (confirmed == true) state.history.clear();
  }

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
              onPressed: state.history.items.isEmpty
                  ? null
                  : () => _confirmClear(context),
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
                padding: const EdgeInsets.all(AppSpace.md),
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
    final text = Theme.of(context).textTheme;
    final sending = item.direction == TransferDirection.sending;
    final statusColor = item.success ? AppColors.success : scheme.error;

    return Padding(
      padding: const EdgeInsets.only(bottom: AppSpace.sm),
      child: Card(
        child: Padding(
          padding: const EdgeInsets.all(AppSpace.sm),
          child: Row(
            children: [
              Container(
                width: 44,
                height: 44,
                decoration: BoxDecoration(
                  gradient: LinearGradient(
                    begin: Alignment.topLeft,
                    end: Alignment.bottomRight,
                    colors: [
                      statusColor.withValues(alpha: 0.22),
                      statusColor.withValues(alpha: 0.10),
                    ],
                  ),
                  borderRadius: BorderRadius.circular(AppRadius.md),
                ),
                child: Icon(
                  item.success
                      ? (sending
                            ? Icons.upload_rounded
                            : Icons.download_rounded)
                      : Icons.error_outline_rounded,
                  color: statusColor,
                ),
              ),
              const Gap(AppSpace.sm),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      item.fileName,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: text.titleSmall?.copyWith(
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                    const Gap(AppSpace.xxs),
                    Text(
                      '${sending ? 'Sent to' : 'Received from'} ${item.peerName} · '
                      '${formatBytes(item.bytes)} · ${_ago(item.at)}'
                      '${item.success ? '' : ' · Failed'}',
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: text.bodySmall?.copyWith(
                        color: scheme.onSurfaceVariant,
                      ),
                    ),
                  ],
                ),
              ),
            ],
          ),
        ),
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
