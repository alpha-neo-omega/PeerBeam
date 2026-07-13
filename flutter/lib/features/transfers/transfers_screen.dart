import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../state/app_scope.dart';
import '../../state/models.dart';
import '../../widgets/appear.dart';
import '../../widgets/common.dart';

/// Active transfers with animated progress and per-transfer controls. Listens
/// to the transfer store only.
class TransfersScreen extends StatelessWidget {
  const TransfersScreen({super.key});

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    return Scaffold(
      appBar: AppBar(title: const Text('Transfers')),
      body: SafeArea(
        child: ContentPane(
          child: AnimatedBuilder(
            animation: state.transfer,
            builder: (context, _) {
              final items = state.transfer.transfers;
              if (items.isEmpty) {
                return const EmptyState(
                  icon: Icons.swap_horiz_rounded,
                  title: 'No active transfers',
                  message: 'Files you send or receive will show up here.',
                );
              }
              return ListView.builder(
                padding: const EdgeInsets.all(16),
                itemCount: items.length,
                itemBuilder: (context, i) => Appear(
                  index: i,
                  child: Padding(
                    padding: const EdgeInsets.only(bottom: 12),
                    child: _TransferCard(transfer: items[i]),
                  ),
                ),
              );
            },
          ),
        ),
      ),
    );
  }
}

class _TransferCard extends StatelessWidget {
  final Transfer transfer;
  const _TransferCard({required this.transfer});

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    final sending = transfer.direction == TransferDirection.sending;
    final paused = transfer.state == TransferState.paused;

    return Card(
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                CircleAvatar(
                  backgroundColor: scheme.primaryContainer,
                  child: Icon(
                    sending ? Icons.upload_rounded : Icons.download_rounded,
                    color: scheme.onPrimaryContainer,
                  ),
                ),
                const SizedBox(width: 12),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        transfer.fileName,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style:
                            text.titleSmall?.copyWith(fontWeight: FontWeight.w600),
                      ),
                      Text(
                        '${sending ? 'To' : 'From'} ${transfer.peerName} · ${transfer.state.label}',
                        style: text.bodySmall
                            ?.copyWith(color: scheme.onSurfaceVariant),
                      ),
                    ],
                  ),
                ),
              ],
            ),
            const SizedBox(height: 14),
            TweenAnimationBuilder<double>(
              tween: Tween(begin: 0, end: transfer.progress),
              duration: AppMotion.slow,
              curve: AppMotion.curve,
              builder: (context, value, _) => ClipRRect(
                borderRadius: BorderRadius.circular(8),
                child: LinearProgressIndicator(
                  value: value,
                  minHeight: 8,
                  backgroundColor: scheme.surfaceContainerHighest,
                ),
              ),
            ),
            const SizedBox(height: 8),
            Row(
              children: [
                Text(
                  '${formatBytes(transfer.doneBytes)} / ${formatBytes(transfer.totalBytes)}',
                  style: text.bodySmall
                      ?.copyWith(color: scheme.onSurfaceVariant),
                ),
                const Spacer(),
                IconButton(
                  tooltip: paused ? 'Resume' : 'Pause',
                  onPressed: () => paused
                      ? state.transfer.resume(transfer.id)
                      : state.transfer.pause(transfer.id),
                  icon: Icon(
                    paused ? Icons.play_arrow_rounded : Icons.pause_rounded,
                  ),
                ),
                IconButton(
                  tooltip: 'Cancel',
                  onPressed: () => state.transfer.cancel(transfer.id),
                  icon: const Icon(Icons.close_rounded),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }
}
