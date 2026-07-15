import 'dart:io';

import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../platform/open_path.dart';
import '../../features/send/send_text.dart';
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

  /// A text message (sent/received via the message flow) vs a real file.
  bool get _isMessage => messageFileName.hasMatch(item.fileName);

  /// Files/folders: open with the OS handler. Messages: show the text + Copy.
  Future<void> _tap(BuildContext context) async {
    if (_isMessage) {
      String content;
      try {
        content = await File(item.path).readAsString();
      } catch (_) {
        content = '';
      }
      if (!context.mounted) return;
      if (content.trim().isEmpty) {
        ScaffoldMessenger.of(context)
          ..hideCurrentSnackBar()
          ..showSnackBar(
            const SnackBar(
              content: Text('Message content is no longer available'),
            ),
          );
        return;
      }
      final dir = item.direction == TransferDirection.sending ? 'to' : 'from';
      await showMessageDialog(
        context,
        title: 'Message $dir ${item.peerName}',
        text: content,
      );
      return;
    }
    final error = await openLocalPath(item.path);
    if (error != null && context.mounted) {
      ScaffoldMessenger.of(context)
        ..hideCurrentSnackBar()
        ..showSnackBar(SnackBar(content: Text(error)));
    }
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    final sending = item.direction == TransferDirection.sending;
    final message = _isMessage;
    final statusColor = item.success ? AppColors.success : scheme.error;
    final icon = !item.success
        ? Icons.error_outline_rounded
        : message
        ? Icons.chat_bubble_outline_rounded
        : (sending ? Icons.upload_rounded : Icons.download_rounded);

    return Padding(
      padding: const EdgeInsets.only(bottom: AppSpace.sm),
      child: Card(
        child: InkWell(
          onTap: () => _tap(context),
          child: Padding(
            padding: const EdgeInsets.all(AppSpace.sm),
            child: Row(
              children: [
                CircleAvatar(
                  radius: 22,
                  backgroundColor: statusColor.withValues(alpha: 0.15),
                  child: Icon(icon, size: AppIcons.md, color: statusColor),
                ),
                const Gap(AppSpace.sm),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        message ? 'Text message' : item.fileName,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: text.titleSmall?.copyWith(
                          fontWeight: FontWeight.w600,
                        ),
                      ),
                      const Gap(AppSpace.xxs),
                      Text(
                        message
                            ? '${sending ? 'Sent to' : 'Received from'} ${item.peerName} · '
                                  '${_ago(item.at)} · tap to copy'
                                  '${item.success ? '' : ' · Failed'}'
                            : '${sending ? 'Sent to' : 'Received from'} ${item.peerName} · '
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
