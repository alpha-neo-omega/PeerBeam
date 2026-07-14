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
/// online) and saved (by-address) devices. Returns null when dismissed or when
/// nothing is available (a snackbar explains).
Future<PickedTarget?> showDevicePicker(BuildContext context) async {
  final scope = AppScope.of(context);
  final online = scope.device.devices.where((d) => d.online).toList();
  final saved = scope.saved.devices;

  if (online.isEmpty && saved.isEmpty) {
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(const SnackBar(content: Text('No devices to send to')));
    return null;
  }

  return showModalBottomSheet<PickedTarget>(
    context: context,
    showDragHandle: true,
    builder: (ctx) {
      final scheme = Theme.of(ctx).colorScheme;
      final label = Theme.of(
        ctx,
      ).textTheme.labelLarge?.copyWith(color: scheme.onSurfaceVariant);
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
