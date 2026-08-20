import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../sdk/error_text.dart';
import '../../sdk/models.dart';
import '../../sdk/peerbeam.dart';
import '../../state/app_scope.dart';
import '../../state/models.dart';
import '../../state/stores.dart';
import '../../widgets/appear.dart';
import '../../widgets/common.dart';
import '../../widgets/status_dot.dart';
import 'device_actions.dart';
import 'wake_dialog.dart';

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
class DevicesScreen extends StatefulWidget {
  const DevicesScreen({super.key});

  @override
  State<DevicesScreen> createState() => _DevicesScreenState();
}

class _DevicesScreenState extends State<DevicesScreen> {
  /// The ids the user has marked as their own, or null while that read has not
  /// answered — see [_mineError] for why the two are not collapsed into an
  /// empty set. "None of these are yours" is a claim about a list, and making
  /// it before the list has arrived is precisely how a failed read starts
  /// reading as a fact.
  Set<String>? _mine;

  /// The last failure of that read. Kept separate so the screen can say "we
  /// could not find out" — an ungrouped list with a retry — instead of
  /// rendering an empty **My devices** group, which would answer the question
  /// the read failed to answer.
  Object? _mineError;

  @override
  void initState() {
    super.initState();
    // Post-frame, not in initState: `AppScope.of` needs a mounted element, and
    // the write path below reaches ScaffoldMessenger for the same reason.
    WidgetsBinding.instance.addPostFrameCallback((_) => _loadMine());
  }

  Future<void> _loadMine() async {
    final api = AppScope.of(context).api;
    if (api == null) {
      // No engine: nothing can be marked and nothing can be read, so an empty
      // set is the truth here rather than a stand-in for one.
      setState(() {
        _mine = const {};
        _mineError = null;
      });
      return;
    }
    setState(() {
      _mine = null;
      _mineError = null;
    });
    try {
      final ids = await api.myDevices();
      if (!mounted) return;
      setState(() => _mine = ids.toSet());
    } catch (e) {
      if (!mounted) return;
      setState(() => _mineError = e);
    }
  }

  /// Add or drop the label, then re-read.
  ///
  /// Re-read rather than adopting the tap: the engine's list is what the
  /// grouping claims to show, and a write it declined must not leave the screen
  /// displaying something that did not happen.
  Future<void> _setMine(Device device, bool mine) async {
    final api = AppScope.of(context).api;
    if (api == null) return;
    final messenger = ScaffoldMessenger.of(context);
    try {
      await api.setDeviceMine(device.id, mine: mine);
    } catch (e) {
      messenger
        ..hideCurrentSnackBar()
        ..showSnackBar(SnackBar(content: Text(friendlyError(e))));
      return;
    }
    await _loadMine();
    if (!mounted) return;
    messenger
      ..hideCurrentSnackBar()
      ..showSnackBar(
        SnackBar(
          // The confirmation carries the claim, because this is the moment a
          // user is most likely to believe they just granted something: the
          // word "mine" invites it, and nothing else on screen contradicts it.
          content: Text(
            mine
                ? '${device.name} is grouped under My devices. That is a label '
                      'kept on this device — it grants no permission, and '
                      '${device.name} is not told.'
                : '${device.name} is no longer under My devices. Nothing else '
                      'changed; the label never granted anything.',
          ),
        ),
      );
  }

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
          if (devices.isEmpty) return const _Empty();
          // Composed as a flat list of rows — banner, headers, cards — so the
          // section structure lives in one readable place instead of index
          // arithmetic inside an item builder. The list is one network's worth
          // of devices, and only the visible rows are mounted.
          final rows = _rows(state, devices);
          return ContentPane(
            child: ListView.builder(
              padding: const EdgeInsets.all(AppSpace.md),
              itemCount: rows.length,
              itemBuilder: (context, i) => rows[i],
            ),
          );
        },
      ),
    );
  }

  List<Widget> _rows(AppState state, List<Device> devices) {
    final mine = _mine;
    final rows = <Widget>[
      _SharingBanner(sharing: state.settings.sharePresence),
    ];

    final error = _mineError;
    if (error != null) {
      rows.add(_GroupingUnavailable(error: error, onRetry: _loadMine));
      rows.addAll(_cards(state, devices, mine: null, from: 0));
      return rows;
    }
    if (mine == null) {
      rows.add(const _CheckingOwnership());
      rows.addAll(_cards(state, devices, mine: null, from: 0));
      return rows;
    }

    final ours = [
      for (final d in devices)
        if (mine.contains(d.id)) d,
    ];
    if (ours.isEmpty) {
      // No group yet, so no header for one: a heading over nothing reads as a
      // section that failed to load. One line instead, which both makes the
      // feature findable — it lives behind a row menu — and puts the label's
      // "grants nothing" on screen before anyone uses it.
      rows.add(const _NothingMarked());
      rows.addAll(_cards(state, devices, mine: mine, from: 0));
      return rows;
    }
    final theirs = [
      for (final d in devices)
        if (!mine.contains(d.id)) d,
    ];
    rows
      ..add(const SectionHeader(title: 'My devices'))
      ..add(const _MineExplanation())
      ..addAll(_cards(state, ours, mine: mine, from: 0));
    if (theirs.isNotEmpty) {
      rows
        ..add(const SectionHeader(title: 'Other devices'))
        ..addAll(_cards(state, theirs, mine: mine, from: ours.length));
    }
    return rows;
  }

  /// One card per device. [from] continues the entrance stagger across a
  /// section boundary so the two groups cascade as one list.
  List<Widget> _cards(
    AppState state,
    List<Device> devices, {
    required Set<String>? mine,
    required int from,
  }) {
    final PeerBeamApi? api = state.api;
    return [
      for (var i = 0; i < devices.length; i++)
        Padding(
          padding: const EdgeInsets.only(bottom: AppSpace.xs),
          child: Appear(
            index: from + i,
            child: DeviceStatusCard(
              device: devices[i],
              presence: state.presence.of(devices[i].id),
              actions: DeviceActions(
                device: devices[i],
                mine: mine?.contains(devices[i].id),
                onSetMine: api == null
                    ? null
                    : (value) => _setMine(devices[i], value),
                onWake: api == null
                    ? null
                    : () =>
                          showWakeDialog(context, api: api, device: devices[i]),
              ),
            ),
          ),
        ),
    ];
  }
}

class _Empty extends StatelessWidget {
  const _Empty();

  @override
  Widget build(BuildContext context) => const EmptyState(
    icon: Icons.devices_other_rounded,
    title: 'No devices yet',
    message: 'Devices appear here as they are discovered on your network.',
  );
}

/// The **My devices** heading's one job beyond naming the group.
///
/// It is a label kept on this device: it grants nothing, widens no permission,
/// and the devices in it are never told they are in it. A machine grouped here
/// with no Browse permission still has no Browse permission. Stated in the
/// group itself because grouping is the feature's whole visible effect, and a
/// heading that implied more would break the most important claim PeerBeam
/// makes about it.
class _MineExplanation extends StatelessWidget {
  const _MineExplanation();

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.only(
        left: AppSpace.xxs,
        right: AppSpace.xxs,
        bottom: AppSpace.xs,
      ),
      child: Text(
        'A label kept on this device. It grants nothing, widens no permission, '
        'and these devices are never told.',
        style: theme.textTheme.bodySmall?.copyWith(
          color: theme.colorScheme.onSurfaceVariant,
        ),
      ),
    );
  }
}

/// The default: no machine marked as the user's own.
class _NothingMarked extends StatelessWidget {
  const _NothingMarked();

  @override
  Widget build(BuildContext context) => const _Line(
    icon: Icons.label_outline_rounded,
    text:
        'None of these are marked as yours. Mark one from its menu to group it '
        'under My devices — a label kept on this device that grants nothing '
        'and is never sent anywhere.',
  );
}

/// Shown while the ownership read is in flight, so the ungrouped list below is
/// visibly provisional rather than an answer.
class _CheckingOwnership extends StatelessWidget {
  const _CheckingOwnership();

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.only(bottom: AppSpace.sm),
      child: Row(
        children: [
          const SizedBox.square(
            dimension: 14,
            child: CircularProgressIndicator(strokeWidth: 2),
          ),
          const Gap(AppSpace.xs),
          Expanded(
            child: Text(
              'Checking which of these are yours…',
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// The ownership read failed.
///
/// A card rather than a full-page [ErrorState]: the device list itself arrived
/// over the event stream and is perfectly good, so replacing it would hide
/// working information to report a failed one. What must not happen is the
/// grouping quietly claiming nothing is marked, which is why this says so in
/// as many words and the list below carries no headers.
class _GroupingUnavailable extends StatelessWidget {
  final Object error;
  final VoidCallback onRetry;

  const _GroupingUnavailable({required this.error, required this.onRetry});

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.only(bottom: AppSpace.sm),
      child: Card(
        color: theme.colorScheme.errorContainer,
        child: Padding(
          padding: const EdgeInsets.all(AppSpace.sm),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Icon(
                Icons.error_outline_rounded,
                size: AppIcons.sm,
                color: theme.colorScheme.onErrorContainer,
              ),
              const Gap(AppSpace.xs),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      'Could not read which devices are yours',
                      style: theme.textTheme.titleSmall?.copyWith(
                        color: theme.colorScheme.onErrorContainer,
                      ),
                    ),
                    Text(
                      '${friendlyError(error)} The list below is not grouped — '
                      'this is not a claim that none of them are yours.',
                      style: theme.textTheme.bodySmall?.copyWith(
                        color: theme.colorScheme.onErrorContainer,
                      ),
                    ),
                    const Gap(AppSpace.xs),
                    TextButton.icon(
                      // Coloured explicitly: the button's default foreground is
                      // `primary`, which is a colour chosen against `surface`
                      // and not against the error tint this card is painted in.
                      style: TextButton.styleFrom(
                        foregroundColor: theme.colorScheme.onErrorContainer,
                      ),
                      onPressed: onRetry,
                      icon: const Icon(Icons.refresh_rounded),
                      label: const Text('Try again'),
                    ),
                  ],
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// A quiet icon + sentence row, the shape the list's non-card notes share.
class _Line extends StatelessWidget {
  final IconData icon;
  final String text;

  const _Line({required this.icon, required this.text});

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.only(bottom: AppSpace.sm),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Icon(
            icon,
            size: AppIcons.sm,
            color: theme.colorScheme.onSurfaceVariant,
          ),
          const Gap(AppSpace.xs),
          Expanded(
            child: Text(
              text,
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
            ),
          ),
        ],
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

  /// The row's menu (mark as mine, wake), or null for a card with nothing to
  /// act on. Injected rather than built here so this stays a pure rendering of
  /// facts about a device — it is also rendered in tests with no engine and no
  /// [AppScope] above it.
  final Widget? actions;

  const DeviceStatusCard({
    super.key,
    required this.device,
    this.presence,
    this.actions,
  });

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
                ?actions,
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
