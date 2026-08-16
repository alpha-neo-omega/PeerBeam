import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../data/transfer_repository.dart';
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
              // Only worth a banner from two upwards: with a single waiting
              // transfer its own card's Accept is already the shortest path,
              // and a banner over one card is just a second button saying the
              // same thing.
              final waiting = state.transfer.awaitingApproval.length;
              return Column(
                children: [
                  if (waiting >= 2)
                    Padding(
                      padding: const EdgeInsets.fromLTRB(
                        AppSpace.md,
                        AppSpace.md,
                        AppSpace.md,
                        0,
                      ),
                      child: _BulkApprovalBanner(waiting: waiting),
                    ),
                  Expanded(
                    child: ListView.builder(
                      padding: const EdgeInsets.all(AppSpace.md),
                      itemCount: items.length,
                      itemBuilder: (context, i) => Appear(
                        key: ValueKey(items[i].id),
                        index: i,
                        child: Padding(
                          padding: const EdgeInsets.only(bottom: AppSpace.sm),
                          child: _TransferCard(transfer: items[i]),
                        ),
                      ),
                    ),
                  ),
                ],
              );
            },
          ),
        ),
      ),
    );
  }
}

/// `1 file` / `3 files`. Shared by the banner's heading and its result report
/// so the two can never describe the same batch differently.
String _files(int n) => '$n ${n == 1 ? 'file' : 'files'}';

/// A verified report of what a bulk approval actually did.
///
/// Every number here comes from a decision the engine answered — never from
/// the count that happened to be on screen when the button was tapped. Between
/// the render and the tap a transfer can time out, its sender can give up, or
/// the user can answer it from its own card, and saying "Accepted 5" when two
/// of them were already gone would be a claim, not a fact.
String _bulkReport(String verbPast, BulkDecision d) {
  if (d.requested == 0) return 'Nothing was waiting';
  if (d.settled == d.requested) return '$verbPast ${_files(d.settled)}';
  if (d.settled == 0 && d.failed == 0) return 'None were still waiting';
  final why = <String>[
    if (d.gone > 0)
      '${d.gone} ${d.gone == 1 ? 'was' : 'were'} no longer waiting',
    if (d.failed > 0) "${d.failed} couldn't be ${verbPast.toLowerCase()}",
  ];
  return '$verbPast ${d.settled} of ${d.requested} — ${why.join(', ')}';
}

/// Shown above the list when **two or more** inbound transfers are waiting on
/// the user: one decision for the batch instead of a tap per card. With one
/// waiting transfer its own card is already the shortest path, so the banner
/// stays hidden rather than duplicating it.
///
/// Deliberately two actions, not three. The cards keep Decline / Accept /
/// **Trust**; the banner offers accept and decline only. Trusting a device is a
/// lasting grant of auto-accept for everything it sends from then on — a
/// materially stronger act than approving the batch on screen — so it stays a
/// per-device choice made on purpose. There is no "Trust all", and nothing
/// here is remembered for next time (I6).
class _BulkApprovalBanner extends StatefulWidget {
  /// How many inbound transfers are awaiting approval right now.
  final int waiting;
  const _BulkApprovalBanner({required this.waiting});

  @override
  State<_BulkApprovalBanner> createState() => _BulkApprovalBannerState();
}

class _BulkApprovalBannerState extends State<_BulkApprovalBanner> {
  /// A batch decision is in flight. Both actions are disabled until it lands,
  /// so a second tap cannot re-answer ids the first has already settled and
  /// then report them back as "no longer waiting".
  bool _busy = false;

  Future<void> _decideBatch({required bool accepting}) async {
    if (_busy) return;
    final messenger = ScaffoldMessenger.of(context);
    final transfers = AppScope.of(context).transfer;
    setState(() => _busy = true);
    try {
      // `acceptAll`, never anything that trusts.
      final result = accepting
          ? await transfers.acceptAll()
          : await transfers.declineAll();
      messenger
        ..hideCurrentSnackBar()
        ..showSnackBar(
          SnackBar(
            content: Text(
              _bulkReport(accepting ? 'Accepted' : 'Declined', result),
            ),
          ),
        );
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    return Card(
      color: scheme.secondaryContainer,
      child: Padding(
        padding: const EdgeInsets.all(AppSpace.md),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Icon(
                  Icons.download_rounded,
                  size: AppIcons.md,
                  color: scheme.onSecondaryContainer,
                ),
                const Gap(AppSpace.sm),
                Expanded(
                  child: Text(
                    '${_files(widget.waiting)} waiting for approval',
                    style: text.titleSmall?.copyWith(
                      fontWeight: FontWeight.w600,
                      color: scheme.onSecondaryContainer,
                    ),
                  ),
                ),
              ],
            ),
            const Gap(AppSpace.xs),
            Align(
              alignment: Alignment.centerRight,
              child: Wrap(
                alignment: WrapAlignment.end,
                spacing: AppSpace.xs,
                runSpacing: AppSpace.xs,
                children: [
                  TextButton(
                    onPressed: _busy
                        ? null
                        : () => _decideBatch(accepting: false),
                    child: const Text('Decline all'),
                  ),
                  FilledButton(
                    onPressed: _busy
                        ? null
                        : () => _decideBatch(accepting: true),
                    child: const Text('Accept all'),
                  ),
                ],
              ),
            ),
          ],
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

/// Progress meta line: `done / total · speed · ETA`. Speed/ETA only while
/// actively transferring (and only when the engine reports them).
String _meta(Transfer t) {
  final parts = <String>[
    '${formatBytes(t.doneBytes)} / ${formatBytes(t.totalBytes)}',
  ];
  if (t.state == TransferState.transferring) {
    final speed = formatSpeed(t.speedBps);
    final eta = formatEta(t.etaSecs);
    if (speed.isNotEmpty) parts.add(speed);
    if (eta.isNotEmpty) parts.add(eta);
  }
  return parts.join(' · ');
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
                  CircleAvatar(
                    radius: 22,
                    backgroundColor: accent.withValues(alpha: 0.15),
                    child: Icon(
                      sending ? Icons.upload_rounded : Icons.download_rounded,
                      size: AppIcons.md,
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
                            Text(
                              transfer.state.label,
                              style: text.labelSmall?.copyWith(
                                color: accent,
                                fontWeight: FontWeight.w600,
                              ),
                            ),
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
              // A `Wrap` (not a `Row`) so the action cluster can drop to its
              // own line on narrow widths instead of overflowing — the
              // awaitingApproval case has three actions (Decline/Accept/
              // Trust) where the old two-button row used to just fit.
              Wrap(
                alignment: WrapAlignment.spaceBetween,
                crossAxisAlignment: WrapCrossAlignment.center,
                spacing: AppSpace.xs,
                runSpacing: AppSpace.xs,
                children: [
                  Text(
                    _meta(transfer),
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: text.bodySmall?.copyWith(
                      color: scheme.onSurfaceVariant,
                    ),
                  ),
                  Wrap(
                    alignment: WrapAlignment.end,
                    spacing: AppSpace.xs,
                    runSpacing: AppSpace.xs,
                    children: awaitingApproval
                        ? [
                            TextButton(
                              onPressed: () =>
                                  state.transfer.reject(transfer.id),
                              child: const Text('Decline'),
                            ),
                            FilledButton.tonal(
                              onPressed: () =>
                                  state.transfer.accept(transfer.id),
                              child: const Text('Accept'),
                            ),
                            Tooltip(
                              message:
                                  'Accept and always trust this device',
                              child: FilledButton(
                                onPressed: () => state.transfer.acceptTrust(
                                  transfer.id,
                                ),
                                child: const Text('Trust'),
                              ),
                            ),
                          ]
                        : [
                            IconButton(
                              tooltip: paused ? 'Resume' : 'Pause',
                              onPressed: () => paused
                                  ? state.transfer.resume(transfer.id)
                                  : state.transfer.pause(transfer.id),
                              icon: Icon(
                                paused
                                    ? Icons.play_arrow_rounded
                                    : Icons.pause_rounded,
                              ),
                            ),
                            IconButton(
                              tooltip: 'Cancel',
                              onPressed: () =>
                                  state.transfer.cancel(transfer.id),
                              icon: const Icon(Icons.close_rounded),
                            ),
                          ],
                  ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }
}
