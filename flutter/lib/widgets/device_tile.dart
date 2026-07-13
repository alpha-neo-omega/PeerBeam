import 'package:flutter/material.dart';

import '../state/models.dart';
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

    final reachText =
        device.reach.map((r) => r.label).join(' and ');
    final latency = device.latencyMs != null ? ', ${device.latencyMs} ms' : '';
    final semantic =
        '${device.name}, ${device.kind.label}, ${device.online ? 'online' : 'offline'}, '
        'reachable via $reachText$latency';

    return MergeSemantics(
      child: Semantics(
        button: true,
        label: semantic,
        child: Card(
          child: InkWell(
            onTap: device.online ? onSend : null,
            child: Padding(
              padding: const EdgeInsets.all(12),
              child: Row(
                children: [
                  _Avatar(device: device),
                  const SizedBox(width: 14),
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          device.name,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: text.titleSmall
                              ?.copyWith(fontWeight: FontWeight.w600),
                        ),
                        const SizedBox(height: 4),
                        Text(
                          device.online
                              ? '${device.kind.label} · Online$latency'
                              : '${device.kind.label} · Offline',
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: text.bodySmall
                              ?.copyWith(color: scheme.onSurfaceVariant),
                        ),
                        const SizedBox(height: 8),
                        Wrap(
                          spacing: 6,
                          runSpacing: 4,
                          children: [
                            for (final r in device.reach) _ReachChip(reach: r),
                          ],
                        ),
                      ],
                    ),
                  ),
                  const SizedBox(width: 8),
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
      width: 48,
      height: 48,
      child: Stack(
        clipBehavior: Clip.none,
        children: [
          Container(
            width: 48,
            height: 48,
            decoration: BoxDecoration(
              color: scheme.primaryContainer,
              borderRadius: BorderRadius.circular(14),
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
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 3),
      decoration: BoxDecoration(
        color: scheme.secondaryContainer,
        borderRadius: BorderRadius.circular(8),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(reach.icon, size: 12, color: scheme.onSecondaryContainer),
          const SizedBox(width: 4),
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
