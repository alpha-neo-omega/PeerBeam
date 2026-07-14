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
                padding: const EdgeInsets.all(AppSpace.md),
                itemCount: items.length,
                itemBuilder: (context, i) => Appear(
                  index: i,
                  child: Padding(
                    padding: const EdgeInsets.only(bottom: AppSpace.sm),
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

/// State → accent colour for the progress bar and status. Kept here (UI-only)
/// so the shared model stays presentation-free.
Color _stateColor(TransferState s, ColorScheme scheme) => switch (s) {
  TransferState.completed => AppColors.success,
  TransferState.failed => scheme.error,
  TransferState.paused => AppColors.warning,
  _ => scheme.primary,
};

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
    final pct = (transfer.progress * 100).round();
    final accent = _stateColor(transfer.state, scheme);
    // An inbound transfer awaiting the user's approval — needs Accept/Decline,
    // not the pause/cancel controls.
    final awaitingApproval =
        !sending && transfer.state == TransferState.pending;

    return Semantics(
      container: true,
      label:
          '${sending ? 'Sending' : 'Receiving'} ${transfer.fileName} '
          '${sending ? 'to' : 'from'} ${transfer.peerName}, '
          '$pct percent, ${transfer.state.label}',
      child: Card(
        child: Padding(
          padding: const EdgeInsets.all(AppSpace.md),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Container(
                    width: 44,
                    height: 44,
                    decoration: BoxDecoration(
                      gradient: LinearGradient(
                        begin: Alignment.topLeft,
                        end: Alignment.bottomRight,
                        colors: [
                          accent.withValues(alpha: 0.22),
                          accent.withValues(alpha: 0.10),
                        ],
                      ),
                      borderRadius: BorderRadius.circular(AppRadius.md),
                    ),
                    child: Icon(
                      sending ? Icons.upload_rounded : Icons.download_rounded,
                      color: accent,
                    ),
                  ),
                  const Gap(AppSpace.sm),
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          transfer.fileName,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: text.titleSmall?.copyWith(
                            fontWeight: FontWeight.w600,
                          ),
                        ),
                        const Gap(AppSpace.xxs),
                        Row(
                          children: [
                            Text(
                              '${sending ? 'To' : 'From'} ${transfer.peerName}',
                              style: text.bodySmall?.copyWith(
                                color: scheme.onSurfaceVariant,
                              ),
                            ),
                            const Gap(AppSpace.xs),
                            _StatusChip(state: transfer.state, accent: accent),
                          ],
                        ),
                      ],
                    ),
                  ),
                  const Gap(AppSpace.xs),
                  Text(
                    '$pct%',
                    style: text.titleMedium?.copyWith(
                      fontWeight: FontWeight.w700,
                      color: accent,
                    ),
                  ),
                ],
              ),
              const Gap(AppSpace.sm),
              TweenAnimationBuilder<double>(
                tween: Tween(begin: 0, end: transfer.progress),
                duration: AppMotion.duration(context, AppMotion.slow),
                curve: AppMotion.curve,
                builder: (context, value, _) => ClipRRect(
                  borderRadius: BorderRadius.circular(AppRadius.sm),
                  child: LinearProgressIndicator(
                    value: value,
                    minHeight: 8,
                    color: accent,
                    backgroundColor: scheme.surfaceContainerHighest,
                  ),
                ),
              ),
              const Gap(AppSpace.xs),
              Row(
                children: [
                  Text(
                    '${formatBytes(transfer.doneBytes)} / ${formatBytes(transfer.totalBytes)}',
                    style: text.bodySmall?.copyWith(
                      color: scheme.onSurfaceVariant,
                    ),
                  ),
                  const Spacer(),
                  if (awaitingApproval) ...[
                    TextButton(
                      onPressed: () => state.transfer.reject(transfer.id),
                      child: const Text('Decline'),
                    ),
                    const Gap(AppSpace.xs),
                    FilledButton(
                      onPressed: () => state.transfer.accept(transfer.id),
                      child: const Text('Accept'),
                    ),
                  ] else ...[
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
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// Compact status pill tinted by the transfer state.
class _StatusChip extends StatelessWidget {
  final TransferState state;
  final Color accent;
  const _StatusChip({required this.state, required this.accent});

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: AppSpace.xs, vertical: 2),
      decoration: BoxDecoration(
        color: accent.withValues(alpha: 0.14),
        borderRadius: BorderRadius.circular(AppRadius.sm),
      ),
      child: Text(
        state.label,
        style: Theme.of(context).textTheme.labelSmall?.copyWith(
          color: accent,
          fontWeight: FontWeight.w700,
        ),
      ),
    );
  }
}
