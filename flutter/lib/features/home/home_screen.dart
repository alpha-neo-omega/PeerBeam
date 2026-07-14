import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../data/saved_devices_repository.dart' show SavedDevice;
import '../../platform/desktop_files.dart';
import '../../sdk/error_text.dart';
import '../../sdk/models.dart' show PeerTarget;
import '../../state/app_scope.dart';
import '../../state/models.dart';
import '../../widgets/appear.dart';
import '../../widgets/common.dart';
import '../../widgets/device_tile.dart';
import '../../widgets/quick_action.dart';
import '../send/staged_sheet.dart';

/// Home — nearby devices, quick actions. Listens to the device store only, so
/// transfer/history changes never rebuild it.
class HomeScreen extends StatelessWidget {
  const HomeScreen({super.key});

  void _todo(BuildContext context, String what) {
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text('$what is coming soon')));
  }

  /// Open a search over discovered devices; on pick, send files to it.
  Future<void> _searchDevices(BuildContext context) async {
    final devices = AppScope.of(context).device.devices;
    final device = await showSearch<Device?>(
      context: context,
      delegate: _DeviceSearchDelegate(devices),
    );
    if (device == null || !context.mounted) return;
    await _sendTo(context, device);
  }

  /// Pick files with the native picker (desktop) and open the staged sheet.
  Future<void> _pickFiles(BuildContext context) async {
    if (!isDesktop) {
      _todo(context, 'Send files');
      return;
    }
    final staging = AppScope.of(context).staging;
    final picked = await pickFilesToStage();
    if (picked.isEmpty || !context.mounted) return;
    final added = staging.add(picked);
    if (added > 0 && context.mounted) {
      showStagedFilesSheet(context, staging);
    }
  }

  /// Send to a manually-entered address (host/IP or MagicDNS name + port).
  /// Covers peers that discovery can't surface — headless servers, or Tailscale
  /// on platforms without a local tailnet API (e.g. Android).
  Future<void> _sendToAddress(BuildContext context) async {
    final scope = AppScope.of(context);
    final target = await _promptForAddress(context);
    if (target == null || !context.mounted) return;
    void snack(String m) => ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(m)));
    final picked = await pickFilesToStage();
    if (picked.isEmpty || !context.mounted) return;
    try {
      await scope.transfer.send(target, picked.map((f) => f.path).toList());
      if (context.mounted) snack('Sending ${picked.length} to ${target.name}');
    } catch (e) {
      if (context.mounted) snack(friendlyError(e));
    }
  }

  /// Dialog to collect a host/IP (or MagicDNS name) and port → [PeerTarget].
  Future<PeerTarget?> _promptForAddress(BuildContext context) {
    final host = TextEditingController();
    final port = TextEditingController(text: '49600');
    return showDialog<PeerTarget>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Send to address'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            TextField(
              controller: host,
              autofocus: true,
              decoration: const InputDecoration(
                labelText: 'Host / IP or MagicDNS name',
                hintText: '100.73.134.21',
              ),
            ),
            const Gap(AppSpace.sm),
            TextField(
              controller: port,
              keyboardType: TextInputType.number,
              decoration: const InputDecoration(labelText: 'Port'),
            ),
          ],
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () {
              final h = host.text.trim();
              final p = int.tryParse(port.text.trim()) ?? 0;
              if (h.isEmpty || p <= 0 || p > 65535) {
                Navigator.pop(context); // invalid → cancel
                return;
              }
              Navigator.pop(
                context,
                PeerTarget(name: h, addresses: [h], port: p),
              );
            },
            child: const Text('Choose file'),
          ),
        ],
      ),
    );
  }

  /// Save a device (name + host/IP or MagicDNS + port) to the persistent book.
  Future<void> _addSavedDevice(BuildContext context) async {
    final scope = AppScope.of(context);
    final name = TextEditingController();
    final host = TextEditingController();
    final port = TextEditingController(text: '49600');
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Add device'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            TextField(
              controller: name,
              autofocus: true,
              decoration: const InputDecoration(
                labelText: 'Name',
                hintText: 'Living-room server',
              ),
            ),
            const Gap(AppSpace.sm),
            TextField(
              controller: host,
              decoration: const InputDecoration(
                labelText: 'Host / IP or MagicDNS name',
                hintText: '100.73.134.21',
              ),
            ),
            const Gap(AppSpace.sm),
            TextField(
              controller: port,
              keyboardType: TextInputType.number,
              decoration: const InputDecoration(labelText: 'Port'),
            ),
          ],
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Save'),
          ),
        ],
      ),
    );
    if (ok != true) return;
    final h = host.text.trim();
    final p = int.tryParse(port.text.trim()) ?? 0;
    final n = name.text.trim().isEmpty ? h : name.text.trim();
    if (h.isEmpty || p <= 0 || p > 65535) return;
    await scope.saved.add(name: n, host: h, port: p);
  }

  /// Pick files and send them to a saved device (real transfer).
  Future<void> _sendToSaved(BuildContext context, SavedDevice d) async {
    final scope = AppScope.of(context);
    void snack(String m) => ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(m)));
    final picked = await pickFilesToStage();
    if (picked.isEmpty || !context.mounted) return;
    final target = PeerTarget(name: d.name, addresses: [d.host], port: d.port);
    try {
      await scope.transfer.send(target, picked.map((f) => f.path).toList());
      if (context.mounted) snack('Sending ${picked.length} to ${d.name}');
    } catch (e) {
      if (context.mounted) snack(friendlyError(e));
    }
  }

  /// Pick files and send them to [device] through the engine (real transfer).
  Future<void> _sendTo(BuildContext context, Device device) async {
    final scope = AppScope.of(context);
    final target = scope.device.peerTarget(device.id);
    void snack(String m) => ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(m)));
    if (target == null) {
      snack('${device.name} is not reachable right now');
      return;
    }
    final picked = await pickFilesToStage();
    if (picked.isEmpty || !context.mounted) return;
    try {
      await scope.transfer.send(target, picked.map((f) => f.path).toList());
      if (context.mounted) snack('Sending ${picked.length} to ${device.name}');
    } catch (e) {
      if (context.mounted) snack(friendlyError(e));
    }
  }

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    final scheme = Theme.of(context).colorScheme;

    return Scaffold(
      body: SafeArea(
        child: Center(
          child: ConstrainedBox(
            constraints: const BoxConstraints(
              maxWidth: Breakpoints.contentMaxWidth,
            ),
            child: AnimatedBuilder(
              animation: Listenable.merge([state.device, state.saved]),
              builder: (context, _) {
                final devices = state.device.devices;
                final saved = state.saved.devices;
                return CustomScrollView(
                  slivers: [
                    SliverAppBar.large(
                      title: const Text('PeerBeam'),
                      actions: [
                        IconButton(
                          icon: const Icon(Icons.dns_rounded),
                          tooltip: 'Send to address',
                          onPressed: () => _sendToAddress(context),
                        ),
                        IconButton(
                          icon: const Icon(Icons.search_rounded),
                          tooltip: 'Search devices',
                          onPressed: () => _searchDevices(context),
                        ),
                        const Gap(AppSpace.xs),
                      ],
                    ),

                    // Quick actions.
                    SliverPadding(
                      padding: const EdgeInsets.fromLTRB(
                        AppSpace.md,
                        0,
                        AppSpace.md,
                        AppSpace.xs,
                      ),
                      sliver: SliverToBoxAdapter(
                        child: Row(
                          children: [
                            Expanded(
                              child: QuickAction(
                                icon: Icons.folder_open_rounded,
                                label: 'Send Files',
                                color: scheme.primary,
                                onTap: () => _pickFiles(context),
                              ),
                            ),
                            const Gap(AppSpace.sm),
                            Expanded(
                              child: QuickAction(
                                icon: Icons.qr_code_2_rounded,
                                label: 'QR Pair',
                                color: scheme.tertiary,
                                onTap: () => _todo(context, 'QR pair'),
                              ),
                            ),
                            const Gap(AppSpace.sm),
                            Expanded(
                              child: QuickAction(
                                icon: Icons.content_paste_rounded,
                                label: 'Clipboard',
                                color: scheme.secondary,
                                onTap: () => _todo(context, 'Clipboard'),
                              ),
                            ),
                          ],
                        ),
                      ),
                    ),

                    // Saved devices — manual/Tailscale-by-address, always
                    // visible so peers discovery can't surface stay reachable.
                    SliverPadding(
                      padding: const EdgeInsets.fromLTRB(
                        AppSpace.md,
                        AppSpace.xs,
                        AppSpace.md,
                        AppSpace.xxs,
                      ),
                      sliver: SliverToBoxAdapter(
                        child: SectionHeader(
                          title: 'Saved Devices',
                          trailing: IconButton.filledTonal(
                            tooltip: 'Add device by address',
                            icon: const Icon(Icons.add_rounded),
                            onPressed: () => _addSavedDevice(context),
                          ),
                        ),
                      ),
                    ),
                    if (saved.isEmpty)
                      SliverToBoxAdapter(
                        child: Padding(
                          padding: const EdgeInsets.fromLTRB(
                            AppSpace.md,
                            0,
                            AppSpace.md,
                            AppSpace.xs,
                          ),
                          child: Text(
                            'Add a device by its address (IP or MagicDNS name) '
                            'to reach it without discovery — e.g. a Tailscale '
                            'peer or a headless server.',
                            style: Theme.of(context).textTheme.bodySmall
                                ?.copyWith(color: scheme.onSurfaceVariant),
                          ),
                        ),
                      )
                    else
                      SliverPadding(
                        padding: const EdgeInsets.fromLTRB(
                          AppSpace.md,
                          0,
                          AppSpace.md,
                          AppSpace.xs,
                        ),
                        sliver: SliverList.builder(
                          itemCount: saved.length,
                          itemBuilder: (context, i) => Appear(
                            index: i,
                            child: Padding(
                              padding: const EdgeInsets.only(bottom: AppSpace.xs),
                              child: _SavedDeviceCard(
                                device: saved[i],
                                onTap: () => _sendToSaved(context, saved[i]),
                                onRemove: () => state.saved.remove(saved[i].id),
                              ),
                            ),
                          ),
                        ),
                      ),

                    // Section header + scan toggle.
                    SliverPadding(
                      padding: const EdgeInsets.fromLTRB(
                        AppSpace.md,
                        AppSpace.xs,
                        AppSpace.md,
                        AppSpace.xxs,
                      ),
                      sliver: SliverToBoxAdapter(
                        child: SectionHeader(
                          title: 'Nearby Devices',
                          trailing: FilledButton.tonalIcon(
                            onPressed: state.device.toggleScan,
                            icon: AnimatedSwitcher(
                              duration: AppMotion.fast,
                              child: Icon(
                                state.device.scanning
                                    ? Icons.stop_rounded
                                    : Icons.refresh_rounded,
                                key: ValueKey(state.device.scanning),
                                size: AppIcons.sm,
                              ),
                            ),
                            label: Text(state.device.scanning ? 'Stop' : 'Scan'),
                          ),
                        ),
                      ),
                    ),

                    if (devices.isEmpty)
                      const SliverFillRemaining(
                        hasScrollBody: false,
                        child: EmptyState(
                          icon: Icons.devices_other_rounded,
                          title: 'No devices yet',
                          message:
                              'Make sure other devices are on the same network '
                              'or tailnet, then scan.',
                        ),
                      )
                    else
                      SliverPadding(
                        padding: const EdgeInsets.fromLTRB(
                          AppSpace.md,
                          AppSpace.xxs,
                          AppSpace.md,
                          AppSpace.xl,
                        ),
                        sliver: SliverGrid.builder(
                          gridDelegate:
                              const SliverGridDelegateWithMaxCrossAxisExtent(
                                maxCrossAxisExtent: 420,
                                mainAxisExtent: 140,
                                crossAxisSpacing: AppSpace.sm,
                                mainAxisSpacing: AppSpace.sm,
                              ),
                          itemCount: devices.length,
                          itemBuilder: (context, i) => Appear(
                            index: i,
                            child: DeviceTile(
                              device: devices[i],
                              onSend: () => _sendTo(context, devices[i]),
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
      ),
    );
  }
}

/// A saved-device card (by-address peer): gradient avatar, address, send-on-tap,
/// remove action. Lifts on hover like the other cards.
class _SavedDeviceCard extends StatelessWidget {
  final SavedDevice device;
  final VoidCallback onTap;
  final VoidCallback onRemove;
  const _SavedDeviceCard({
    required this.device,
    required this.onTap,
    required this.onRemove,
  });

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    return HoverScale(
      child: Card(
        child: InkWell(
          onTap: onTap,
          child: Padding(
            padding: const EdgeInsets.all(AppSpace.sm),
            child: Row(
              children: [
                Container(
                  width: 48,
                  height: 48,
                  decoration: BoxDecoration(
                    gradient: LinearGradient(
                      begin: Alignment.topLeft,
                      end: Alignment.bottomRight,
                      colors: [
                        scheme.secondaryContainer,
                        scheme.secondaryContainer.withValues(alpha: 0.6),
                      ],
                    ),
                    borderRadius: BorderRadius.circular(AppRadius.md),
                  ),
                  child: Icon(
                    Icons.dns_rounded,
                    color: scheme.onSecondaryContainer,
                  ),
                ),
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
                        '${device.host}:${device.port}',
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: text.bodySmall?.copyWith(
                          color: scheme.onSurfaceVariant,
                        ),
                      ),
                    ],
                  ),
                ),
                IconButton(
                  tooltip: 'Remove',
                  icon: const Icon(Icons.delete_outline_rounded),
                  onPressed: onRemove,
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// Searches the discovered-device list by name. Returns the chosen [Device] via
/// `close`, or null when dismissed. Operates on a snapshot passed at open time.
class _DeviceSearchDelegate extends SearchDelegate<Device?> {
  final List<Device> devices;
  _DeviceSearchDelegate(this.devices)
    : super(searchFieldLabel: 'Search devices');

  List<Device> get _matches {
    final q = query.trim().toLowerCase();
    if (q.isEmpty) return devices;
    return devices.where((d) => d.name.toLowerCase().contains(q)).toList();
  }

  @override
  List<Widget> buildActions(BuildContext context) => [
    if (query.isNotEmpty)
      IconButton(
        tooltip: 'Clear',
        icon: const Icon(Icons.clear_rounded),
        onPressed: () => query = '',
      ),
  ];

  @override
  Widget buildLeading(BuildContext context) => IconButton(
    tooltip: 'Back',
    icon: const Icon(Icons.arrow_back_rounded),
    onPressed: () => close(context, null),
  );

  @override
  Widget buildResults(BuildContext context) => _list(context);

  @override
  Widget buildSuggestions(BuildContext context) => _list(context);

  Widget _list(BuildContext context) {
    final matches = _matches;
    if (matches.isEmpty) {
      return const EmptyState(
        icon: Icons.search_off_rounded,
        title: 'No matches',
        message: 'No discovered device matches that name.',
      );
    }
    return ListView.builder(
      padding: const EdgeInsets.all(AppSpace.md),
      itemCount: matches.length,
      itemBuilder: (context, i) => Padding(
        padding: const EdgeInsets.only(bottom: AppSpace.xs),
        child: DeviceTile(
          device: matches[i],
          onSend: () => close(context, matches[i]),
        ),
      ),
    );
  }
}
