import 'package:flutter/material.dart';

import '../../app/theme.dart';
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
            const SizedBox(height: 12),
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
              animation: state.device,
              builder: (context, _) {
                final devices = state.device.devices;
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
                          onPressed: () => _todo(context, 'Search'),
                        ),
                      ],
                    ),

                    // Quick actions.
                    SliverPadding(
                      padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
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
                            const SizedBox(width: 12),
                            Expanded(
                              child: QuickAction(
                                icon: Icons.qr_code_2_rounded,
                                label: 'QR Pair',
                                color: scheme.tertiary,
                                onTap: () => _todo(context, 'QR pair'),
                              ),
                            ),
                            const SizedBox(width: 12),
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

                    // Section header + scan toggle.
                    SliverPadding(
                      padding: const EdgeInsets.fromLTRB(16, 8, 16, 4),
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
                                size: 18,
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
                          title: 'No devices yet',
                          message:
                              'Make sure other devices are on the same network '
                              'or tailnet, then scan.',
                        ),
                      )
                    else
                      SliverPadding(
                        padding: const EdgeInsets.fromLTRB(16, 4, 16, 24),
                        sliver: SliverGrid.builder(
                          gridDelegate:
                              const SliverGridDelegateWithMaxCrossAxisExtent(
                                maxCrossAxisExtent: 420,
                                mainAxisExtent: 132,
                                crossAxisSpacing: 12,
                                mainAxisSpacing: 12,
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
