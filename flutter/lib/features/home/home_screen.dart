import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../platform/desktop_files.dart';
import '../../state/app_scope.dart';
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
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(content: Text('$what — wiring lands with the engine bridge')),
    );
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

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    final scheme = Theme.of(context).colorScheme;

    return Scaffold(
      body: SafeArea(
        child: Center(
          child: ConstrainedBox(
            constraints:
                const BoxConstraints(maxWidth: Breakpoints.contentMaxWidth),
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
                              onSend: () =>
                                  _todo(context, 'Send to ${devices[i].name}'),
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
