import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../sdk/models.dart';
import '../../state/app_scope.dart';
import '../../state/models.dart';
import '../../widgets/appear.dart';
import '../../widgets/status_dot.dart';

/// The device dashboard: every known device, what it can do, and whatever
/// status it has chosen to share.
///
/// **Why a nav destination rather than richer device tiles on Home.** Home's
/// tiles are a two-line grid cell sized by a computed `mainAxisExtent`, and
/// they exist to answer one question — *which device do I send this to?* The
/// dashboard answers a different one: *how are my devices doing?* It carries up
/// to nine facts per device (name, form factor, platform, online, last-seen,
/// route, battery, storage, version), which does not fit that cell without
/// either overflowing it or shrinking the send affordance Home is built around.
/// Splitting them keeps each view honest about its job, and it means a device
/// that shares nothing still has somewhere to appear with its identity and
/// reachability instead of being a row of blank gauges on the send screen.
class DevicesScreen extends StatelessWidget {
  const DevicesScreen({super.key});

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    return Scaffold(
      appBar: AppBar(title: const Text('Devices')),
      body: AnimatedBuilder(
        animation: Listenable.merge([
          state.device,
          state.presence,
          state.settings,
        ]),
        builder: (context, _) {
          final devices = state.device.devices;
          if (devices.isEmpty) {
            return const _Empty();
          }
          return ListView.builder(
            padding: const EdgeInsets.all(AppSpace.md),
            itemCount: devices.length + 1,
            itemBuilder: (context, i) {
              if (i == 0) {
                return _SharingBanner(sharing: state.settings.sharePresence);
              }
              final device = devices[i - 1];
              return Appear(
                index: i - 1,
                child: DeviceStatusCard(
                  device: device,
                  presence: state.presence.of(device.id),
                ),
              );
            },
          );
        },
      ),
    );
  }
}

class _Empty extends StatelessWidget {
  const _Empty();

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(AppSpace.xl),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(Icons.devices_other_rounded, size: 48, color: scheme.outline),
            const Gap(AppSpace.md),
            Text('No devices yet', style: Theme.of(context).textTheme.titleMedium),
            const Gap(AppSpace.xs),
            Text(
              'Devices appear here as they are discovered on your network.',
              textAlign: TextAlign.center,
              style: Theme.of(
                context,
              ).textTheme.bodySmall?.copyWith(color: scheme.onSurfaceVariant),
            ),
          ],
        ),
      ),
    );
  }
}

/// One line of honest copy about what this device is sending, and to whom.
class _SharingBanner extends StatelessWidget {
  final bool sharing;
  const _SharingBanner({required this.sharing});

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Padding(
      padding: const EdgeInsets.only(bottom: AppSpace.sm),
      child: Row(
        children: [
          Icon(
            sharing ? Icons.visibility_rounded : Icons.visibility_off_rounded,
            size: 16,
            color: scheme.onSurfaceVariant,
          ),
          const Gap(AppSpace.xs),
          Expanded(
            child: Text(
              sharing
                  ? 'Sharing this device’s status with trusted devices.'
                  : 'Not sharing this device’s status. You still see what others share.',
              style: Theme.of(
                context,
              ).textTheme.bodySmall?.copyWith(color: scheme.onSurfaceVariant),
            ),
          ),
        ],
      ),
    );
  }
}

/// A device's identity and reachability, plus any status it shared.
///
/// The card is deliberately readable top-to-bottom in two halves: what we know
/// about the device regardless (always present), then what it chose to tell us
/// (often absent). A device that shares nothing renders the first half and one
/// line of explanation — **never a row of zeroed gauges**, which would claim a
/// dead battery and a full disk that were never measured.
///
/// The round-trip chip belongs to the first half even though it sits among the
/// second's, and the distinction matters: it is **our** measurement of **our**
/// link to that device, taken by the transport, not something the peer
/// disclosed. So it renders for a device that shares nothing at all, and it is
/// absent — never zero, never a guess — for a device we have not connected to.
class DeviceStatusCard extends StatelessWidget {
  final Device device;
  final SdkPresence? presence;

  const DeviceStatusCard({super.key, required this.device, this.presence});

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    final p = presence;
    final shared = p != null && p.hasAny;

    return Card(
      child: Padding(
        padding: const EdgeInsets.all(AppSpace.md),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Stack(
                  clipBehavior: Clip.none,
                  children: [
                    CircleAvatar(
                      backgroundColor: scheme.secondaryContainer,
                      foregroundColor: scheme.onSecondaryContainer,
                      child: Icon(device.kind.icon, size: 20),
                    ),
                    Positioned(
                      right: -2,
                      bottom: -2,
                      child: StatusDot(online: device.online),
                    ),
                  ],
                ),
                const Gap(AppSpace.sm),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Text(
                        device.name,
                        style: text.titleSmall,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                      ),
                      Text(
                        _identityLine(device),
                        style: text.bodySmall?.copyWith(
                          color: scheme.onSurfaceVariant,
                        ),
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                      ),
                    ],
                  ),
                ),
              ],
            ),
            const Gap(AppSpace.sm),
            Wrap(
              spacing: AppSpace.xs,
              runSpacing: AppSpace.xs,
              children: [
                // Ours, not the peer's: the round trip the transport measured
                // on the last connection to this device. Outside the `shared`
                // branch on purpose — a device that discloses nothing can
                // still be one we have measured a link to.
                if (device.latencyMs != null)
                  _Chip(
                    icon: Icons.speed_rounded,
                    label: '${formatLatency(device.latencyMs!)} round trip',
                    tooltip:
                        'Measured by this device on its last connection to '
                        '${device.name}.',
                  ),
                // Each chip below is rendered only when its field actually
                // arrived. Absent is absent — there is no zero fallback
                // anywhere here.
                if (shared && p.batteryPercent != null)
                  _Chip(
                    icon: p.charging == true
                        ? Icons.battery_charging_full_rounded
                        : _batteryIcon(p.batteryPercent!),
                    label: p.charging == true
                        ? '${p.batteryPercent}% charging'
                        : '${p.batteryPercent}%',
                  ),
                if (shared && p.storageFreeBytes != null)
                  _Chip(
                    icon: Icons.sd_storage_rounded,
                    label: '${formatBytes(p.storageFreeBytes!)} free',
                  ),
                if (shared && p.network != null)
                  _Chip(
                    icon: _networkIcon(p.network!),
                    label: networkLabel(p.network!),
                  ),
                if (shared && p.appVersion != null)
                  _Chip(icon: Icons.tag_rounded, label: 'v${p.appVersion}'),
              ],
            ),
            if (!shared) ...[
              if (device.latencyMs != null) const Gap(AppSpace.xs),
              Text(
                'Status not shared',
                style: text.bodySmall?.copyWith(color: scheme.outline),
              ),
            ],
            if (shared) ...[
              const Gap(AppSpace.xs),
              Text(
                // Counted from OUR receipt time — peer clocks are not
                // synchronised, so the sender's own timestamp is not truth.
                'Updated ${formatAge(p.ageSeconds)}',
                style: text.labelSmall?.copyWith(color: scheme.outline),
              ),
            ],
          ],
        ),
      ),
    );
  }
}

/// Form factor, platform, and how the device is reachable — the facts that
/// hold whether or not the device shares anything.
///
/// The round trip used to be appended here as a bare "23 ms", which said
/// nothing about what was measured or by whom; it is a labelled chip now.
String _identityLine(Device d) {
  final parts = <String>[
    d.kind.label,
    platformLabel(d.platform),
    if (d.online) ...d.reach.map((r) => r.label) else 'Offline',
  ];
  return parts.join(' · ');
}

/// Engine platform id → display name.
String platformLabel(String platform) => switch (platform) {
  'macos' => 'macOS',
  'ios' => 'iOS',
  'windows' => 'Windows',
  'linux' => 'Linux',
  'android' => 'Android',
  'web' => 'Web',
  _ => platform,
};

/// Wire network word → display name. The set is closed and validated in Rust,
/// so an unrecognised value here means a newer peer word: shown as-is only
/// after Rust has already vetted it against the known set.
String networkLabel(String network) => switch (network) {
  'lan' => 'LAN',
  'wifi' => 'Wi-Fi',
  'ethernet' => 'Ethernet',
  'tailscale' => 'Tailscale',
  'unknown' => 'Unknown link',
  _ => network,
};

IconData _networkIcon(String network) => switch (network) {
  'wifi' => Icons.wifi_rounded,
  'ethernet' => Icons.settings_ethernet_rounded,
  'tailscale' => Icons.shield_rounded,
  'lan' => Icons.lan_rounded,
  _ => Icons.help_outline_rounded,
};

IconData _batteryIcon(int percent) {
  if (percent >= 90) return Icons.battery_full_rounded;
  if (percent >= 60) return Icons.battery_5_bar_rounded;
  if (percent >= 30) return Icons.battery_3_bar_rounded;
  if (percent >= 10) return Icons.battery_2_bar_rounded;
  return Icons.battery_alert_rounded;
}

/// Decimal bytes, matching the CLI's rendering.
String formatBytes(int bytes) {
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  var v = bytes.toDouble();
  var i = 0;
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000;
    i++;
  }
  return i == 0 ? '$bytes B' : '${v.toStringAsFixed(1)} ${units[i]}';
}

/// Seconds since we received a status, phrased relatively.
String formatAge(int seconds) {
  if (seconds < 60) return 'just now';
  final minutes = seconds ~/ 60;
  if (minutes < 60) return '${minutes}m ago';
  final hours = minutes ~/ 60;
  if (hours < 24) return '${hours}h ago';
  return '${hours ~/ 24}d ago';
}

class _Chip extends StatelessWidget {
  final IconData icon;
  final String label;

  /// Where the number came from, for a chip whose provenance is not obvious
  /// from its wording. Null for the chips that simply restate what a peer
  /// sent.
  final String? tooltip;
  const _Chip({required this.icon, required this.label, this.tooltip});

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final chip = Container(
      padding: const EdgeInsets.symmetric(
        horizontal: AppSpace.xs,
        vertical: AppSpace.xxs,
      ),
      decoration: BoxDecoration(
        color: scheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(AppRadius.sm),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(icon, size: 14, color: scheme.onSurfaceVariant),
          const Gap(AppSpace.xxs),
          Text(
            label,
            style: Theme.of(
              context,
            ).textTheme.labelSmall?.copyWith(color: scheme.onSurfaceVariant),
          ),
        ],
      ),
    );
    final message = tooltip;
    return message == null ? chip : Tooltip(message: message, child: chip);
  }
}
