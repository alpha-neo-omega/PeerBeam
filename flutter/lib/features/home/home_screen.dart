import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../data/saved_devices_repository.dart' show SavedDevice;
import '../../platform/desktop_files.dart';
import '../../sdk/error_text.dart';
import '../../sdk/models.dart' show ChatConversation, PeerTarget;
import '../../state/app_scope.dart';
import '../../state/models.dart';
import '../../state/staging.dart';
import '../../widgets/appear.dart';
import '../../widgets/brand_mark.dart';
import '../../widgets/common.dart';
import '../../widgets/processing.dart';
import '../../widgets/device_tile.dart';
import '../chat/chat_screen.dart';
import '../qr/qr.dart';
import '../send/pick_device.dart';
import '../send/send_staged.dart';
import '../send/send_text.dart';
import '../send/staged_sheet.dart';

/// Home — conversations, nearby devices, quick actions. Listens to the device,
/// saved-device and chat stores only, so transfer/history changes never rebuild
/// it.
class HomeScreen extends StatefulWidget {
  const HomeScreen({super.key});

  @override
  State<HomeScreen> createState() => _HomeScreenState();
}

class _HomeScreenState extends State<HomeScreen> {
  @override
  void initState() {
    super.initState();
    // After the first frame, not in `initState` directly: repositories are
    // constructed before the engine's `initialize()` has been awaited, so an
    // earlier read would just hit `not_initialised` and be swallowed. Same
    // reasoning as the chat screen's own post-frame `openThread`.
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      AppScope.of(context).chat.refreshConversations();
    });
  }

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
    if (!context.mounted) return;
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
    final picked = await withProcessing(
      context,
      'Preparing files…',
      pickFilesToStage,
    );
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
    if (scope.staging.isNotEmpty) {
      await sendStaged(context, target, target.name);
      return;
    }
    await _pickFilesAndSend(context, target, target.name);
  }

  /// Pick files with the native picker and send them straight to [target] (no
  /// staging). Shared tail of `_sendTo`, `_sendToSaved`, and `_sendToAddress`
  /// once each has confirmed the staging stack is empty.
  Future<void> _pickFilesAndSend(
    BuildContext context,
    PeerTarget target,
    String name,
  ) async {
    final scope = AppScope.of(context);
    void snack(String m) => ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(m)));
    final picked = await withProcessing(
      context,
      'Preparing files…',
      pickFilesToStage,
    );
    if (picked.isEmpty || !context.mounted) return;
    try {
      await scope.transfer.send(target, picked.map((f) => f.path).toList());
      if (context.mounted) snack('Sending ${picked.length} to $name');
    } catch (e) {
      if (context.mounted) snack(friendlyError(e));
    }
  }

  /// Dialog to collect a host/IP (or MagicDNS name) and port → [PeerTarget].
  Future<PeerTarget?> _promptForAddress(BuildContext context) async {
    final host = TextEditingController();
    final port = TextEditingController(text: '49600');
    String? error;
    try {
      return await showDialog<PeerTarget>(
        context: context,
        builder: (context) => StatefulBuilder(
          builder: (context, setState) => AlertDialog(
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
                if (error != null) ...[
                  const Gap(AppSpace.sm),
                  Text(
                    error!,
                    style: TextStyle(
                      color: Theme.of(context).colorScheme.error,
                    ),
                  ),
                ],
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
                    setState(
                      () => error =
                          'Enter a host and a port between 1 and 65535',
                    );
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
        ),
      );
    } finally {
      host.dispose();
      port.dispose();
    }
  }

  /// Save a device (name + host/IP or MagicDNS + port) to the persistent book.
  Future<void> _addSavedDevice(BuildContext context) async {
    final scope = AppScope.of(context);
    final name = TextEditingController();
    final host = TextEditingController();
    final port = TextEditingController(text: '49600');
    String? error;
    try {
      final ok = await showDialog<bool>(
        context: context,
        builder: (context) => StatefulBuilder(
          builder: (context, setState) => AlertDialog(
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
                if (error != null) ...[
                  const Gap(AppSpace.sm),
                  Text(
                    error!,
                    style: TextStyle(
                      color: Theme.of(context).colorScheme.error,
                    ),
                  ),
                ],
              ],
            ),
            actions: [
              TextButton(
                onPressed: () => Navigator.pop(context, false),
                child: const Text('Cancel'),
              ),
              FilledButton(
                onPressed: () {
                  final h = host.text.trim();
                  final p = int.tryParse(port.text.trim()) ?? 0;
                  if (h.isEmpty || p <= 0 || p > 65535) {
                    setState(
                      () => error =
                          'Enter a host and a port between 1 and 65535',
                    );
                    return;
                  }
                  Navigator.pop(context, true);
                },
                child: const Text('Save'),
              ),
            ],
          ),
        ),
      );
      if (ok != true) return;
      final h = host.text.trim();
      final p = int.tryParse(port.text.trim()) ?? 0;
      final n = name.text.trim().isEmpty ? h : name.text.trim();
      if (h.isEmpty || p <= 0 || p > 65535) return;
      await scope.saved.add(name: n, host: h, port: p);
    } finally {
      name.dispose();
      host.dispose();
      port.dispose();
    }
  }

  /// Edit a saved device's name/address in place.
  Future<void> _editSavedDevice(BuildContext context, SavedDevice d) async {
    final scope = AppScope.of(context);
    final name = TextEditingController(text: d.name);
    final host = TextEditingController(text: d.host);
    final port = TextEditingController(text: '${d.port}');
    String? error;
    try {
      final ok = await showDialog<bool>(
        context: context,
        builder: (context) => StatefulBuilder(
          builder: (context, setState) => AlertDialog(
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
                if (error != null) ...[
                  const Gap(AppSpace.sm),
                  Text(
                    error!,
                    style: TextStyle(
                      color: Theme.of(context).colorScheme.error,
                    ),
                  ),
                ],
              ],
            ),
            actions: [
              TextButton(
                onPressed: () => Navigator.pop(context, false),
                child: const Text('Cancel'),
              ),
              FilledButton(
                onPressed: () {
                  final h = host.text.trim();
                  final p = int.tryParse(port.text.trim()) ?? 0;
                  if (h.isEmpty || p <= 0 || p > 65535) {
                    setState(
                      () => error =
                          'Enter a host and a port between 1 and 65535',
                    );
                    return;
                  }
                  Navigator.pop(context, true);
                },
                child: const Text('Save'),
              ),
            ],
          ),
        ),
      );
      if (ok != true) return;
      final h = host.text.trim();
      final p = int.tryParse(port.text.trim()) ?? 0;
      final n = name.text.trim().isEmpty ? h : name.text.trim();
      if (h.isEmpty || p <= 0 || p > 65535) return;
      await scope.saved.update(d.id, name: n, host: h, port: p);
    } finally {
      name.dispose();
      host.dispose();
      port.dispose();
    }
  }

  /// Send to a saved device. Content-first (send the stack if non-empty).
  Future<void> _sendToSaved(BuildContext context, SavedDevice d) async {
    final scope = AppScope.of(context);
    final target = PeerTarget(
      id: d.id,
      name: d.name,
      addresses: [d.host],
      port: d.port,
    );
    if (scope.staging.isNotEmpty) {
      await sendStaged(context, target, d.name);
      return;
    }
    await _pickFilesAndSend(context, target, d.name);
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
    await _pickFilesAndSend(context, target, device.name);
  }

  /// Pick a device from the persistent bar and send the current stack.
  Future<void> _pickAndSendFromBar(BuildContext context) async {
    final picked = await showDevicePicker(context);
    if (picked == null || !context.mounted) return;
    await sendStaged(context, picked.target, picked.name);
  }

  /// Open a chat with a discovered device (pushed, not a nav tab — see
  /// task-9 brief for the M1 rationale).
  ///
  /// Not gated on the device being online: the conversation is local history,
  /// so the thread opens either way. Only the *send* needs the peer, and it
  /// reports its own failure. The one hard stop is a device with no address at
  /// all, where there is nothing to send to even when it comes back.
  void _chatWith(BuildContext context, Device device) {
    final target = AppScope.of(context).device.peerTarget(device.id);
    if (target == null) {
      ScaffoldMessenger.of(context)
        ..hideCurrentSnackBar()
        ..showSnackBar(
          SnackBar(content: Text('No address known for ${device.name}')),
        );
      return;
    }
    Navigator.of(context).push(
      MaterialPageRoute(
        builder: (_) => ChatScreen(peerId: device.id, peer: target),
      ),
    );
  }

  /// The discovered device behind a saved (by-address) entry, or null when
  /// discovery cannot currently see it.
  ///
  /// A conversation may only be opened under a peer's **real** device id.
  /// A [SavedDevice]'s id is a locally minted timestamp that the peer has
  /// never heard of: the engine would file our own rows under it while every
  /// inbound record is keyed by the authenticated device id, so replies would
  /// land in a conversation with no entry point, and queued text — flushed per
  /// authenticated peer — could never be delivered at all. So a saved entry
  /// gets a chat action only once it can be resolved to a real identity, and
  /// [_chatWith] then routes it exactly like any discovered device.
  ///
  /// Re-keying a thread client-side is deliberately NOT attempted here; that
  /// needs an engine-side identity for by-address peers.
  Device? _discovered(BuildContext context, SavedDevice d) =>
      AppScope.of(context).device.deviceAtAddress(d.host, d.port);

  /// The best name we can put to a conversation's peer id.
  ///
  /// Discovery first (a live name, kept current), then the trust store (a peer
  /// we have chatted with has almost always been pinned, and that name outlives
  /// discovery), and finally the device id itself — ugly, but the truth, and
  /// never a fabricated "Unknown device" that two different peers would share.
  String _peerName(BuildContext context, String peerId) {
    final state = AppScope.of(context);
    for (final d in state.device.devices) {
      if (d.id == peerId) return d.name;
    }
    for (final t in state.trust.items) {
      if (t.id == peerId && t.name.isNotEmpty) return t.name;
    }
    return peerId;
  }

  /// Open the thread for a conversation row.
  ///
  /// Keyed by the **authenticated peer id the engine returned**, never a
  /// locally minted one: that is the whole reason this list can reach a peer
  /// discovery cannot see, and the bug 2a removed saved-device chat over.
  ///
  /// The send target is discovery's when it has one and an address-less
  /// placeholder otherwise — the thread stays readable either way, and the chat
  /// screen disables its composer rather than accepting messages the engine
  /// would refuse.
  void _openConversation(BuildContext context, String peerId) {
    final state = AppScope.of(context);
    final name = _peerName(context, peerId);
    final target =
        state.device.peerTarget(peerId) ??
        PeerTarget(id: peerId, name: name, addresses: const [], port: 0);
    Navigator.of(context).push(
      MaterialPageRoute(
        builder: (_) => ChatScreen(peerId: peerId, peer: target),
      ),
    );
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
              animation: Listenable.merge([
                state.device,
                state.saved,
                // The conversations list is chat state; a new thread or a new
                // file offer must show up here without a manual refresh.
                state.chat,
              ]),
              builder: (context, _) {
                // Every device the engine currently knows about, online or
                // not: a peer that drops offline stays listed (dimmed, with
                // send disabled — see `DeviceTile`) rather than vanishing,
                // because its conversation is local history that stays worth
                // opening. The engine removes a device outright when it
                // genuinely goes away, and that still drops it from here.
                final devices = state.device.devices.toList();
                final saved = state.saved.devices;
                final conversations = state.chat.conversations;
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

                    // Conversations — every thread on disk, whether or not
                    // discovery can currently see the peer. Without this a
                    // thread is only reachable through a device tile, so a
                    // queued file waiting for an offline peer would have no
                    // entry point at all.
                    //
                    // Hidden entirely when there are none: this is a resume
                    // surface, and an empty header on a fresh install is
                    // clutter advertising nothing.
                    if (conversations.isNotEmpty) ...[
                      const SliverPadding(
                        padding: EdgeInsets.fromLTRB(
                          AppSpace.md,
                          AppSpace.xs,
                          AppSpace.md,
                          AppSpace.xxs,
                        ),
                        sliver: SliverToBoxAdapter(
                          child: SectionHeader(title: 'Conversations'),
                        ),
                      ),
                      SliverPadding(
                        padding: const EdgeInsets.fromLTRB(
                          AppSpace.md,
                          0,
                          AppSpace.md,
                          AppSpace.xs,
                        ),
                        sliver: SliverList.builder(
                          itemCount: conversations.length,
                          itemBuilder: (context, i) => Appear(
                            index: i,
                            child: Padding(
                              padding: const EdgeInsets.only(
                                bottom: AppSpace.xs,
                              ),
                              child: _ConversationCard(
                                conversation: conversations[i],
                                name: _peerName(
                                  context,
                                  conversations[i].peerId,
                                ),
                                onTap: () => _openConversation(
                                  context,
                                  conversations[i].peerId,
                                ),
                              ),
                            ),
                          ),
                        ),
                      ),
                    ],

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
                                // Only when it resolves to a real device id —
                                // see `_discovered`. Null hides the action
                                // rather than offering a thread whose replies
                                // would be filed somewhere else.
                                onChat: switch (_discovered(context, saved[i])) {
                                  final Device found => () =>
                                      _chatWith(context, found),
                                  null => null,
                                },
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
                        sliver: Builder(
                          builder: (context) {
                            final scale = MediaQuery.textScalerOf(
                              context,
                            ).scale(1.0);
                            // Two text lines (~38px) grow with the OS font
                            // size; the row never shrinks below the 44px
                            // avatar. Plus 24px vertical padding and a small
                            // slack for the inter-line Gap(2)/Card insets.
                            final extent =
                                24.0 + math.max(44.0, 38.0 * scale) + 4;
                            return SliverGrid.builder(
                              gridDelegate:
                                  SliverGridDelegateWithMaxCrossAxisExtent(
                                    maxCrossAxisExtent: 420,
                                    // Tight fit at scale 1.0; grows with
                                    // text scale to avoid overflow.
                                    mainAxisExtent: extent,
                                    crossAxisSpacing: AppSpace.sm,
                                    mainAxisSpacing: AppSpace.sm,
                                  ),
                              itemCount: devices.length,
                              itemBuilder: (context, i) => Appear(
                                index: i,
                                child: DeviceTile(
                                  device: devices[i],
                                  onSend: () => _sendTo(context, devices[i]),
                                  onChat: () => _chatWith(context, devices[i]),
                                ),
                              ),
                            );
                          },
                        ),
                      ),

                    // Keep the last row clear of the persistent selection bar,
                    // which overlays the body (Scaffold.bottomSheet does not
                    // inset the content).
                    SliverToBoxAdapter(
                      child: AnimatedBuilder(
                        animation: state.staging,
                        builder: (context, _) =>
                            SizedBox(height: state.staging.count > 0 ? 80 : 0),
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

/// One conversation on Home: who it is with, when it last moved, and — when
/// the engine says so — that it is waiting on the user for a decision.
///
/// **The badge is not an unread count.** `unread_hint` counts inbound file
/// offers still awaiting an accept/decline, which is the only thing about a
/// thread's state PeerBeam can assert without inventing read receipts, so the
/// row says exactly that and never "N unread". A thread full of unread text
/// legitimately shows nothing here.
class _ConversationCard extends StatelessWidget {
  final ChatConversation conversation;

  /// The peer's display name, already resolved by the caller (discovery, then
  /// the trust store, then the device id itself).
  final String name;
  final VoidCallback onTap;
  const _ConversationCard({
    required this.conversation,
    required this.name,
    required this.onTap,
  });

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    final waiting = conversation.unreadHint;
    final last = conversation.lastAt;
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
                backgroundColor: scheme.secondaryContainer,
                child: Icon(
                  Icons.chat_bubble_outline_rounded,
                  size: AppIcons.md,
                  color: scheme.onSecondaryContainer,
                ),
              ),
              const Gap(AppSpace.sm),
              Expanded(
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      name,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: text.titleSmall,
                    ),
                    const Gap(2),
                    Text(
                      switch ((waiting, last)) {
                        (1, _) => '1 file offer needs your attention',
                        (final n, _) when n > 1 =>
                          '$n file offers need your attention',
                        // No decision pending: say when the thread last moved.
                        // A null timestamp is a thread this build could not
                        // read — still listed, with nothing to say about
                        // itself, rather than a fabricated date.
                        (_, final DateTime at) => 'Last message ${formatAgo(at)}',
                        _ => 'No messages to show',
                      },
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: text.bodySmall?.copyWith(
                        color: conversation.needsAttention
                            ? scheme.primary
                            : scheme.onSurfaceVariant,
                      ),
                    ),
                  ],
                ),
              ),
              if (conversation.needsAttention)
                Tooltip(
                  // Deliberately not "unread": these are decisions, not
                  // messages someone has or hasn't looked at.
                  message: waiting == 1
                      ? '1 file offer is waiting for your decision'
                      : '$waiting file offers are waiting for your decision',
                  child: Padding(
                    padding: const EdgeInsets.symmetric(
                      horizontal: AppSpace.sm,
                    ),
                    child: Badge(
                      label: Text('$waiting'),
                      child: Icon(
                        Icons.move_to_inbox_rounded,
                        size: AppIcons.md,
                        color: scheme.primary,
                      ),
                    ),
                  ),
                ),
            ],
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

  /// Null when this saved entry cannot be resolved to a discovered device —
  /// there is then no real peer id to key a conversation by, so no action is
  /// offered at all rather than a dead or misfiling one.
  final VoidCallback? onChat;
  final VoidCallback onShare;
  final VoidCallback onEdit;
  final VoidCallback onRemove;
  const _SavedDeviceCard({
    required this.device,
    required this.onTap,
    required this.onChat,
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
              if (onChat != null)
                IconButton(
                  onPressed: onChat,
                  icon: const Icon(
                    Icons.chat_bubble_outline_rounded,
                    size: AppIcons.sm,
                  ),
                  tooltip: 'Chat with ${device.name}',
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
