import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../sdk/models.dart' show PeerTarget;
import '../../state/app_scope.dart';
import '../../state/stores.dart';
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

  return showModalBottomSheet<PickedTarget>(
    context: context,
    showDragHandle: true,
    // **Rebuilt while it is open, not read once before it opens.**
    //
    // Both lists used to be computed here, above `showModalBottomSheet`, and
    // the builder closed over that snapshot. Discovery is asynchronous and
    // usually still running in the first seconds after launch — so dropping a
    // file straight away opened a sheet saying "No devices to send to" which
    // then *stayed* wrong for as long as it was open, while the device sat
    // visible on Home behind it. The user's only recourse was to dismiss and
    // reopen, with nothing on screen suggesting it.
    //
    // `DiscoveryRepository` and `SavedDevicesRepository` are both
    // `ChangeNotifier`s; the sheet simply was not listening. Merging them means
    // a device appearing, going offline, or being saved is reflected in place —
    // which is what "Open app ↓ See devices ↓ Click ↓ Send" requires of the one
    // screen standing between seeing and sending.
    builder: (ctx) => AnimatedBuilder(
      animation: Listenable.merge([scope.device, scope.saved]),
      builder: (ctx, _) => _sheet(ctx, scope),
    ),
  );
}

/// The picker's contents for the device lists as they are *right now*.
Widget _sheet(BuildContext ctx, AppState scope) {
  final online = scope.device.devices
      .where((d) => d.online && scope.device.peerTarget(d.id) != null)
      .toList();
  final saved = scope.saved.devices;
  {
    {
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
                        id: d.id,
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
    }
  }
}
