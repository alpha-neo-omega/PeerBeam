import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../state/models.dart';

/// The per-device menu on the dashboard: mark it as one of yours, or wake it.
///
/// # Why a menu, and why here
///
/// Marking a device as yours is **a label this machine keeps**. It grants
/// nothing, widens no permission, and the device is never told — a machine you
/// mark as yours that has no Browse permission still has no Browse permission.
/// So it must not look like the per-device permission switches in Settings, and
/// it is deliberately nowhere near them: a control that sits inside that group,
/// or wears a switch of its own, reads as one of them however its label is
/// worded. A menu entry with the claim written beneath it can be read before it
/// is chosen, which is the only moment the distinction matters.
///
/// The wake entry says both of waking's limits in one line for the same reason:
/// a user who opens this menu looking for "turn that machine on" should learn
/// that it is a local-network packet with no acknowledgement *before* the
/// dialog, not instead of it.
class DeviceActions extends StatelessWidget {
  final Device device;

  /// Whether this device is marked as the user's own, or null when the read
  /// that would say so has not answered (or failed). Null disables the entry
  /// rather than defaulting to "not mine": offering "Mark as mine" for a device
  /// that already is would either be a no-op the user cannot explain, or an
  /// unmark they did not ask for.
  final bool? mine;

  /// Flip the label. Null when there is no engine to write to.
  final ValueChanged<bool>? onSetMine;

  /// Open the wake dialog. Null when there is no engine to send through.
  final VoidCallback? onWake;

  /// Look at what this device shares. Null when it has no address to reach —
  /// which is any device discovery cannot currently see.
  final VoidCallback? onBrowse;

  const DeviceActions({
    super.key,
    required this.device,
    required this.mine,
    this.onSetMine,
    this.onWake,
    this.onBrowse,
  });

  /// The claim under the mark/unmark entry — the one place a user reads what
  /// the label does before choosing it, so it says what it does *not* do.
  String _mineDetail(bool marked) {
    if (mine == null) return 'PeerBeam could not read which devices are yours.';
    if (onSetMine == null) return 'The engine is not running.';
    if (marked) return 'Drops the label. Nothing else changes.';
    return 'A label kept on this device. It grants nothing, and '
        '${device.name} is never told.';
  }

  @override
  Widget build(BuildContext context) {
    final marked = mine ?? false;
    return PopupMenuButton<VoidCallback>(
      icon: const Icon(Icons.more_vert_rounded),
      tooltip: 'Actions for ${device.name}',
      onSelected: (action) => action(),
      itemBuilder: (context) => [
        PopupMenuItem<VoidCallback>(
          // Null `mine` means the grouping read did not answer; the entry stays
          // visible so the menu does not change shape, and explains itself.
          enabled: mine != null && onSetMine != null,
          value: onSetMine == null ? () {} : () => onSetMine!(!marked),
          child: _Entry(
            icon: marked
                ? Icons.label_off_outlined
                : Icons.label_outline_rounded,
            title: marked ? 'Remove from My devices' : 'Mark as mine',
            detail: _mineDetail(marked),
          ),
        ),
        // **The only way in for a discovered device.** Browsing used to be
        // offered solely from a saved by-address entry's menu, so a folder
        // someone shared with you could be reached only by hand-typing their
        // IP — the one thing this app is for not needing. The screen, the
        // engine call and the permission all existed; nothing linked them.
        PopupMenuItem<VoidCallback>(
          enabled: onBrowse != null,
          value: onBrowse ?? () {},
          child: _Entry(
            icon: Icons.folder_shared_outlined,
            title: 'Shared folders…',
            detail: onBrowse == null
                ? 'Only while the device is reachable.'
                : 'What ${device.name} shares with you, if anything.',
          ),
        ),
        PopupMenuItem<VoidCallback>(
          enabled: onWake != null,
          value: onWake ?? () {},
          child: _Entry(
            icon: Icons.power_settings_new_rounded,
            title: 'Wake…',
            detail: onWake == null
                ? 'The engine is not running.'
                : 'Sends a local-network packet. Nothing confirms it arrived.',
          ),
        ),
      ],
    );
  }
}

/// A menu entry with the sentence that keeps it honest underneath it.
///
/// Width-capped: the details are sentences, and a popup menu sizes to its
/// widest child, so uncapped they would drag the menu across the screen.
class _Entry extends StatelessWidget {
  final IconData icon;
  final String title;
  final String detail;

  const _Entry({required this.icon, required this.title, required this.detail});

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return ConstrainedBox(
      constraints: const BoxConstraints(maxWidth: 260),
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: AppSpace.xs),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Icon(icon, size: AppIcons.sm),
            const Gap(AppSpace.sm),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                mainAxisSize: MainAxisSize.min,
                children: [
                  Text(title, style: theme.textTheme.bodyMedium),
                  Text(
                    detail,
                    style: theme.textTheme.bodySmall?.copyWith(
                      color: theme.colorScheme.onSurfaceVariant,
                    ),
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}
