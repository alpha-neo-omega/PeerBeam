import 'package:flutter/material.dart';

import '../app/theme.dart';
import '../state/models.dart';
import 'common.dart';
import 'status_dot.dart';

/// A device row: identity, live status, reach capabilities, and a send action.
/// Semantics are merged into one meaningful announcement.
class DeviceTile extends StatelessWidget {
  final Device device;
  final VoidCallback? onSend;
  const DeviceTile({super.key, required this.device, this.onSend});

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;

    final reachText = device.reach.map((r) => r.label).join(' and ');
    final latency = device.latencyMs != null ? ', ${device.latencyMs} ms' : '';
    final semantic =
        '${device.name}, ${device.kind.label}, ${device.online ? 'online' : 'offline'}, '
        'reachable via $reachText$latency';

    return MergeSemantics(
      child: Semantics(
        button: true,
        label: semantic,
        child: HoverScale(
          child: Card(
            child: InkWell(
              onTap: device.online ? onSend : null,
              // Offline devices are dimmed so reachable ones stand out.
              child: Opacity(
                opacity: device.online ? 1 : 0.5,
                child: Padding(
                  padding: const EdgeInsets.all(AppSpace.sm),
                  child: Row(
                    children: [
                      _Avatar(device: device),
                      const Gap(AppSpace.sm),
                      Expanded(
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            Text(
                              device.name,
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: text.titleSmall?.copyWith(
                                fontWeight: FontWeight.w600,
                              ),
                            ),
                            const Gap(AppSpace.xxs),
                            Text(
                              device.online
                                  ? '${device.kind.label} · Online$latency'
                                  : '${device.kind.label} · Offline',
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: text.bodySmall?.copyWith(
                                color: scheme.onSurfaceVariant,
                              ),
                            ),
                            const Gap(AppSpace.xs),
                            Wrap(
                              spacing: AppSpace.xs,
                              runSpacing: AppSpace.xxs,
                              children: [
                                for (final r in device.reach)
                                  _ReachChip(reach: r),
                              ],
                            ),
                          ],
                        ),
                      ),
                      const Gap(AppSpace.xs),
                      IconButton.filledTonal(
                        onPressed: device.online ? onSend : null,
                        icon: const Icon(Icons.send_rounded),
                        tooltip: 'Send to ${device.name}',
                      ),
                    ],
                  ),
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

class _Avatar extends StatelessWidget {
  final Device device;
  const _Avatar({required this.device});

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return SizedBox(
      width: 52,
      height: 52,
      child: Stack(
        clipBehavior: Clip.none,
        children: [
          Container(
            width: 52,
            height: 52,
            decoration: BoxDecoration(
              gradient: LinearGradient(
                begin: Alignment.topLeft,
                end: Alignment.bottomRight,
                colors: [
                  scheme.primaryContainer,
                  scheme.primaryContainer.withValues(alpha: 0.6),
                ],
              ),
              borderRadius: BorderRadius.circular(AppRadius.lg),
            ),
            child: Icon(device.kind.icon, color: scheme.onPrimaryContainer),
          ),
          Positioned(
            right: -4,
            bottom: -4,
            child: Container(
              padding: const EdgeInsets.all(2),
              decoration: BoxDecoration(
                color: scheme.surface,
                shape: BoxShape.circle,
              ),
              child: StatusDot(online: device.online),
            ),
          ),
        ],
      ),
    );
  }
}

class _ReachChip extends StatelessWidget {
  final Reach reach;
  const _ReachChip({required this.reach});

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: AppSpace.xs, vertical: 3),
      decoration: BoxDecoration(
        color: scheme.secondaryContainer,
        borderRadius: BorderRadius.circular(AppRadius.sm),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(reach.icon, size: 12, color: scheme.onSecondaryContainer),
          const Gap(AppSpace.xxs),
          Text(
            reach.label,
            style: Theme.of(context).textTheme.labelSmall?.copyWith(
              color: scheme.onSecondaryContainer,
              fontWeight: FontWeight.w600,
            ),
          ),
        ],
      ),
    );
  }
}
