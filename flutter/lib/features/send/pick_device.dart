import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../sdk/models.dart' show PeerTarget;
import '../../state/app_scope.dart';
import '../../state/models.dart';

/// A resolved send destination chosen by the user.
class PickedTarget {
  final PeerTarget target;
  final String name;
  const PickedTarget(this.target, this.name);
}

/// Bottom-sheet picker over every reachable destination: nearby (discovered,
/// online) and saved (by-address) devices. Returns null when dismissed. When
/// nothing is available it opens with an empty state — shown *in* the sheet so
/// it's visible above any sheet that opened it (a snackbar would sit behind).
Future<PickedTarget?> showDevicePicker(BuildContext context) async {
  final scope = AppScope.of(context);
  final online = scope.device.devices
      .where((d) => d.online && scope.device.peerTarget(d.id) != null)
      .toList();
  final saved = scope.saved.devices;

  return showModalBottomSheet<PickedTarget>(
    context: context,
    showDragHandle: true,
    builder: (ctx) {
      final scheme = Theme.of(ctx).colorScheme;
      final text = Theme.of(ctx).textTheme;
      final label = text.labelLarge?.copyWith(color: scheme.onSurfaceVariant);
      if (online.isEmpty && saved.isEmpty) {
        return SafeArea(
          child: Padding(
            padding: const EdgeInsets.fromLTRB(
              AppSpace.lg,
              AppSpace.sm,
              AppSpace.lg,
              AppSpace.xl,
            ),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(
                  Icons.devices_other_rounded,
                  size: AppIcons.lg,
                  color: scheme.onSurfaceVariant,
                ),
                const Gap(AppSpace.sm),
                Text('No devices to send to', style: text.titleMedium),
                const Gap(AppSpace.xxs),
                Text(
                  'Scan a QR or add a device by address, then try again.',
                  textAlign: TextAlign.center,
                  style: text.bodyMedium?.copyWith(
                    color: scheme.onSurfaceVariant,
                  ),
                ),
              ],
            ),
          ),
        );
      }
      return SafeArea(
        child: ListView(
          shrinkWrap: true,
          padding: const EdgeInsets.only(bottom: AppSpace.md),
          children: [
            if (online.isNotEmpty) ...[
              Padding(
                padding: const EdgeInsets.fromLTRB(
                  AppSpace.lg,
                  AppSpace.xxs,
                  AppSpace.lg,
                  AppSpace.xxs,
                ),
                child: Text('Nearby', style: label),
              ),
              for (final d in online)
                ListTile(
                  leading: Icon(d.kind.icon),
                  title: Text(d.name),
                  subtitle: Text(d.kind.label),
                  onTap: () {
                    final t = scope.device.peerTarget(d.id);
                    Navigator.pop(
                      ctx,
                      t == null ? null : PickedTarget(t, d.name),
                    );
                  },
                ),
            ],
            if (saved.isNotEmpty) ...[
              Padding(
                padding: const EdgeInsets.fromLTRB(
                  AppSpace.lg,
                  AppSpace.xs,
                  AppSpace.lg,
                  AppSpace.xxs,
                ),
                child: Text('Saved', style: label),
              ),
              for (final d in saved)
                ListTile(
                  leading: const Icon(Icons.dns_rounded),
                  title: Text(d.name),
                  subtitle: Text('${d.host}:${d.port}'),
                  onTap: () => Navigator.pop(
                    ctx,
                    PickedTarget(
                      PeerTarget(
                        name: d.name,
                        addresses: [d.host],
                        port: d.port,
                      ),
                      d.name,
                    ),
                  ),
                ),
            ],
          ],
        ),
      );
    },
  );
}
