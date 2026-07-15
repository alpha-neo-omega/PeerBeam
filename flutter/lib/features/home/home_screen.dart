import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../data/saved_devices_repository.dart' show SavedDevice;
import '../../platform/desktop_files.dart';
import '../../sdk/error_text.dart';
import '../../sdk/models.dart' show PeerTarget;
import '../../state/app_scope.dart';
import '../../state/models.dart';
import '../../state/staging.dart';
import '../../widgets/appear.dart';
import '../../widgets/brand_mark.dart';
import '../../widgets/common.dart';
import '../../widgets/device_tile.dart';
import '../qr/qr.dart';
import '../send/pick_device.dart';
import '../send/send_staged.dart';
import '../send/send_text.dart';
import '../send/staged_sheet.dart';

/// Home — nearby devices, quick actions. Listens to the device store only, so
/// transfer/history changes never rebuild it.
class HomeScreen extends StatelessWidget {
  const HomeScreen({super.key});

  /// Open a search over discovered devices; on pick, send files to it.
  Future<void> _searchDevices(BuildContext context) async {
    final devices = AppScope.of(
      context,
    ).device.devices.where((d) => d.online).toList();
    final device = await showSearch<Device?>(
      context: context,
      delegate: _DeviceSearchDelegate(devices),
    );
    if (device == null || !context.mounted) return;
    await _sendTo(context, device);
  }

  /// Scan a peer's QR (mobile only — needs a camera) and save it as a device.
  Future<void> _scanQr(BuildContext context) async {
    final scope = AppScope.of(context);
    void snack(String m) => ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(m)));
    if (isDesktop) {
      snack('QR scanning needs a camera — use a mobile device');
      return;
    }
    final payload = await openQrScanner(context);
    if (payload == null || !context.mounted) return;
    await scope.saved.add(
      name: payload.name,
      host: payload.host,
      port: payload.port,
    );
    snack('Added ${payload.name}');
  }

  /// Share a saved device's address as a QR for another phone to scan.
  Future<void> _shareSaved(BuildContext context, SavedDevice d) {
    return showShareQrDialog(
      context,
      QrPayload(name: d.name, host: d.host, port: d.port),
    );
  }

  /// Pick a folder (desktop) and stage it for sending.
  Future<void> _pickFolder(BuildContext context) async {
    final staging = AppScope.of(context).staging;
    final folder = await pickFolderToStage();
    if (folder == null || !context.mounted) return;
    if (staging.add([folder]) > 0 && context.mounted) {
      showStagedFilesSheet(context, staging);
    }
  }

  /// Pick files with the native picker and open the staged sheet. Works on
  /// desktop and Android (file_selector copies picks to app storage there).
  Future<void> _pickFiles(BuildContext context) async {
    final staging = AppScope.of(context).staging;
    final picked = await pickFilesToStage();
    if (picked.isEmpty || !context.mounted) return;
    final added = staging.add(picked);
    if (added > 0 && context.mounted) {
      showStagedFilesSheet(context, staging);
    }
  }

  /// Send to a manually-entered address (host/IP or MagicDNS name + port).
  /// Content-first: send the stack if non-empty, else pick files.
  Future<void> _sendToAddress(BuildContext context) async {
    final scope = AppScope.of(context);
    final target = await _promptForAddress(context);
    if (target == null || !context.mounted) return;
    void snack(String m) => ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(m)));
    if (scope.staging.isNotEmpty) {
      await sendStaged(context, target, target.name);
      return;
    }
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
            child: const Text('Next'),
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
              decoration: const InputDecoration(labelText: 'Name'),
            ),
            const Gap(AppSpace.sm),
            TextField(
              controller: host,
              decoration: const InputDecoration(
                labelText: 'Host / IP or MagicDNS name',
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

  /// Edit a saved device's name/address in place.
  Future<void> _editSavedDevice(BuildContext context, SavedDevice d) async {
    final scope = AppScope.of(context);
    final name = TextEditingController(text: d.name);
    final host = TextEditingController(text: d.host);
    final port = TextEditingController(text: '${d.port}');
    final ok = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Edit device'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            TextField(
              controller: name,
              autofocus: true,
              decoration: const InputDecoration(labelText: 'Name'),
            ),
            const Gap(AppSpace.sm),
            TextField(
              controller: host,
              decoration: const InputDecoration(
                labelText: 'Host / IP or MagicDNS name',
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
    await scope.saved.update(d.id, name: n, host: h, port: p);
  }

  /// Send to a saved device. Content-first (send the stack if non-empty).
  Future<void> _sendToSaved(BuildContext context, SavedDevice d) async {
    final scope = AppScope.of(context);
    void snack(String m) => ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(m)));
    final target = PeerTarget(name: d.name, addresses: [d.host], port: d.port);
    if (scope.staging.isNotEmpty) {
      await sendStaged(context, target, d.name);
      return;
    }
    final picked = await pickFilesToStage();
    if (picked.isEmpty || !context.mounted) return;
    try {
      await scope.transfer.send(target, picked.map((f) => f.path).toList());
      if (context.mounted) snack('Sending ${picked.length} to ${d.name}');
    } catch (e) {
      if (context.mounted) snack(friendlyError(e));
    }
  }

  /// Send to a discovered device. Content-first: if the stack has items, send
  /// the whole stack; otherwise pick files and send those.
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
    if (scope.staging.isNotEmpty) {
      await sendStaged(context, target, device.name);
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

  /// Pick a device from the persistent bar and send the current stack.
  Future<void> _pickAndSendFromBar(BuildContext context) async {
    final picked = await showDevicePicker(context);
    if (picked == null || !context.mounted) return;
    await sendStaged(context, picked.target, picked.name);
  }

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    final scheme = Theme.of(context).colorScheme;

    return Scaffold(
      bottomSheet: _SelectionBar(
        staging: state.staging,
        onOpen: () => showStagedFilesSheet(context, state.staging),
        onSend: () => _pickAndSendFromBar(context),
      ),
      body: SafeArea(
        child: Center(
          child: ConstrainedBox(
            constraints: const BoxConstraints(
              maxWidth: Breakpoints.contentMaxWidth,
            ),
            child: AnimatedBuilder(
              animation: Listenable.merge([state.device, state.saved]),
              builder: (context, _) {
                // Nearby shows live peers only — a device that drops offline
                // disappears (the engine still tracks it; the CLI can list it).
                final devices = state.device.devices
                    .where((d) => d.online)
                    .toList();
                final saved = state.saved.devices;
                return CustomScrollView(
                  slivers: [
                    // Brand only on compact (no rail): the nav rail already
                    // shows the logo + wordmark on wider layouts, so the bar
                    // there carries just its actions — no duplicate "PeerBeam".
                    SliverAppBar(
                      pinned: true,
                      title:
                          MediaQuery.sizeOf(context).width < Breakpoints.compact
                          ? const BrandLockup()
                          : null,
                      actions: [
                        IconButton(
                          icon: const Icon(Icons.dns_rounded),
                          tooltip: 'Send to address',
                          onPressed: () => _sendToAddress(context),
                        ),
                        const Gap(AppSpace.xs),
                      ],
                    ),

                    // Search bar — tap to search discovered devices.
                    SliverPadding(
                      padding: const EdgeInsets.fromLTRB(
                        AppSpace.md,
                        0,
                        AppSpace.md,
                        AppSpace.md,
                      ),
                      sliver: SliverToBoxAdapter(
                        child: _SearchPill(
                          onTap: () => _searchDevices(context),
                        ),
                      ),
                    ),

                    // Actions: one hero (send) and two secondary.
                    SliverPadding(
                      padding: const EdgeInsets.fromLTRB(
                        AppSpace.md,
                        0,
                        AppSpace.md,
                        AppSpace.xs,
                      ),
                      sliver: SliverToBoxAdapter(
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.stretch,
                          children: [
                            FilledButton.icon(
                              onPressed: () => _pickFiles(context),
                              style: FilledButton.styleFrom(
                                minimumSize: const Size.fromHeight(56),
                              ),
                              icon: const Icon(Icons.folder_open_rounded),
                              label: const Text('Send files'),
                            ),
                            const Gap(AppSpace.sm),
                            Row(
                              children: [
                                // Desktop: folder send (no camera anyway).
                                // Mobile: QR scan.
                                Expanded(
                                  child: isDesktop
                                      ? FilledButton.tonalIcon(
                                          onPressed: () => _pickFolder(context),
                                          style: FilledButton.styleFrom(
                                            minimumSize: const Size.fromHeight(
                                              48,
                                            ),
                                          ),
                                          icon: const Icon(
                                            Icons.folder_copy_rounded,
                                            size: AppIcons.sm,
                                          ),
                                          label: const Text('Send folder'),
                                        )
                                      : FilledButton.tonalIcon(
                                          onPressed: () => _scanQr(context),
                                          style: FilledButton.styleFrom(
                                            minimumSize: const Size.fromHeight(
                                              48,
                                            ),
                                          ),
                                          icon: const Icon(
                                            Icons.qr_code_scanner_rounded,
                                            size: AppIcons.sm,
                                          ),
                                          label: const Text('Scan QR'),
                                        ),
                                ),
                                const Gap(AppSpace.sm),
                                Expanded(
                                  child: FilledButton.tonalIcon(
                                    onPressed: () => addTextToStack(context),
                                    style: FilledButton.styleFrom(
                                      minimumSize: const Size.fromHeight(48),
                                    ),
                                    icon: const Icon(
                                      Icons.chat_bubble_outline_rounded,
                                      size: AppIcons.sm,
                                    ),
                                    label: const Text('Send text'),
                                  ),
                                ),
                              ],
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
                          title: 'Saved devices',
                          trailing: IconButton(
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
                            'Reach servers and Tailscale peers by address.',
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
                              padding: const EdgeInsets.only(
                                bottom: AppSpace.xs,
                              ),
                              child: _SavedDeviceCard(
                                device: saved[i],
                                onTap: () => _sendToSaved(context, saved[i]),
                                onShare: () => _shareSaved(context, saved[i]),
                                onEdit: () =>
                                    _editSavedDevice(context, saved[i]),
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
                          title: 'Nearby devices',
                          trailing: FilledButton.tonalIcon(
                            onPressed: state.device.toggleScan,
                            style: FilledButton.styleFrom(
                              visualDensity: VisualDensity.compact,
                              padding: const EdgeInsets.symmetric(
                                horizontal: AppSpace.md,
                                vertical: AppSpace.xs,
                              ),
                            ),
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
                            label: Text(
                              state.device.scanning ? 'Stop' : 'Scan',
                            ),
                          ),
                        ),
                      ),
                    ),

                    if (devices.isEmpty)
                      const SliverFillRemaining(
                        hasScrollBody: false,
                        child: EmptyState(
                          icon: Icons.devices_other_rounded,
                          title: 'No nearby devices',
                          message: 'Devices on your network appear here.',
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
                                // Tight fit for the two-line tile.
                                mainAxisExtent: 76,
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

/// The tappable search pill under the app bar — looks like a Material search
/// bar, opens the device search on tap.
class _SearchPill extends StatelessWidget {
  final VoidCallback onTap;
  const _SearchPill({required this.onTap});

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Semantics(
      button: true,
      label: 'Search devices',
      child: Material(
        color: scheme.surfaceContainerHigh,
        shape: const StadiumBorder(),
        child: InkWell(
          onTap: onTap,
          customBorder: const StadiumBorder(),
          child: Padding(
            padding: const EdgeInsets.symmetric(
              horizontal: AppSpace.md,
              vertical: AppSpace.sm + 2,
            ),
            child: Row(
              children: [
                Icon(Icons.search_rounded, color: scheme.onSurfaceVariant),
                const Gap(AppSpace.sm),
                Text(
                  'Search devices',
                  style: Theme.of(context).textTheme.bodyLarge?.copyWith(
                    color: scheme.onSurfaceVariant,
                  ),
                ),
              ],
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
  final VoidCallback onShare;
  final VoidCallback onEdit;
  final VoidCallback onRemove;
  const _SavedDeviceCard({
    required this.device,
    required this.onTap,
    required this.onShare,
    required this.onEdit,
    required this.onRemove,
  });

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    return Card(
      child: InkWell(
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.symmetric(
            horizontal: AppSpace.sm,
            vertical: AppSpace.xs,
          ),
          child: Row(
            children: [
              CircleAvatar(
                radius: 22,
                backgroundColor: scheme.primaryContainer,
                child: Icon(
                  Icons.dns_rounded,
                  size: AppIcons.md,
                  color: scheme.onPrimaryContainer,
                ),
              ),
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
              PopupMenuButton<String>(
                tooltip: 'Device actions',
                onSelected: (v) => switch (v) {
                  'share' => onShare(),
                  'edit' => onEdit(),
                  _ => onRemove(),
                },
                itemBuilder: (_) => const [
                  PopupMenuItem(
                    value: 'share',
                    child: ListTile(
                      leading: Icon(Icons.qr_code_2_rounded),
                      title: Text('Share via QR'),
                    ),
                  ),
                  PopupMenuItem(
                    value: 'edit',
                    child: ListTile(
                      leading: Icon(Icons.edit_rounded),
                      title: Text('Edit'),
                    ),
                  ),
                  PopupMenuItem(
                    value: 'remove',
                    child: ListTile(
                      leading: Icon(Icons.delete_outline_rounded),
                      title: Text('Remove'),
                    ),
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

/// Slim bar pinned to the bottom of Home while the selection stack is
/// non-empty: item count + total, tap to open the tray, Send to pick a device.
class _SelectionBar extends StatelessWidget {
  final StagingStore staging;
  final VoidCallback onOpen;
  final VoidCallback onSend;
  const _SelectionBar({
    required this.staging,
    required this.onOpen,
    required this.onSend,
  });

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    return AnimatedBuilder(
      animation: staging,
      builder: (context, _) {
        final n = staging.count;
        return AnimatedSize(
          duration: AppMotion.fast,
          curve: AppMotion.curve,
          child: n == 0
              ? const SizedBox(width: double.infinity)
              : Material(
                  color: scheme.surfaceContainerHigh,
                  child: SafeArea(
                    top: false,
                    child: InkWell(
                      onTap: onOpen,
                      child: Padding(
                        padding: const EdgeInsets.fromLTRB(
                          AppSpace.md,
                          AppSpace.sm,
                          AppSpace.sm,
                          AppSpace.sm,
                        ),
                        child: Row(
                          children: [
                            Icon(Icons.layers_rounded, color: scheme.primary),
                            const Gap(AppSpace.sm),
                            Expanded(
                              child: Text(
                                '$n ${n == 1 ? 'item' : 'items'} · ${formatBytes(staging.totalBytes)}',
                                style: text.titleSmall,
                              ),
                            ),
                            FilledButton.icon(
                              onPressed: onSend,
                              icon: const Icon(
                                Icons.send_rounded,
                                size: AppIcons.sm,
                              ),
                              label: const Text('Send'),
                            ),
                          ],
                        ),
                      ),
                    ),
                  ),
                ),
        );
      },
    );
  }
}
