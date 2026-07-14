import 'package:flutter/material.dart';

import '../app/theme.dart';
import '../state/models.dart';
import 'status_dot.dart';

/// A device row: identity, live status, and a send action. Reach and latency
/// fold into the subtitle. Semantics merge into one announcement.
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

    final subtitle = device.online
        ? [
            device.kind.label,
            ...device.reach.map((r) => r.label),
            if (device.latencyMs != null) '${device.latencyMs} ms',
          ].join(' · ')
        : '${device.kind.label} · Offline';

    return MergeSemantics(
      child: Semantics(
        button: true,
        label: semantic,
        child: Card(
          child: InkWell(
            onTap: device.online ? onSend : null,
            // Offline devices are dimmed so reachable ones stand out.
            child: Opacity(
              opacity: device.online ? 1 : 0.5,
              child: Padding(
                padding: const EdgeInsets.symmetric(
                  horizontal: AppSpace.sm,
                  vertical: AppSpace.sm,
                ),
                child: Row(
                  children: [
                    _Avatar(device: device),
                    const Gap(AppSpace.sm),
                    Expanded(
                      child: Column(
                        mainAxisSize: MainAxisSize.min,
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(
                            device.name,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: text.titleSmall,
                          ),
                          const Gap(2),
                          Text(
                            subtitle,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: text.bodySmall?.copyWith(
                              color: scheme.onSurfaceVariant,
                            ),
                          ),
                        ],
                      ),
                    ),
                    const Gap(AppSpace.xs),
                    IconButton.filledTonal(
                      onPressed: device.online ? onSend : null,
                      icon: const Icon(Icons.send_rounded, size: AppIcons.sm),
                      tooltip: 'Send to ${device.name}',
                    ),
                  ],
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
      width: 44,
      height: 44,
      child: Stack(
        clipBehavior: Clip.none,
        children: [
          CircleAvatar(
            radius: 22,
            backgroundColor: scheme.primaryContainer,
            child: Icon(
              device.kind.icon,
              size: AppIcons.md,
              color: scheme.onPrimaryContainer,
            ),
          ),
          Positioned(
            right: -2,
            bottom: -2,
            child: Container(
              padding: const EdgeInsets.all(2),
              decoration: BoxDecoration(
                color: scheme.surface,
                shape: BoxShape.circle,
              ),
              child: StatusDot(online: device.online, size: 8),
            ),
          ),
        ],
      ),
    );
  }
}
