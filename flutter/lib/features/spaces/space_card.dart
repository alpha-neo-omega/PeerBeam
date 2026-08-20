import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../sdk/models.dart' show Space;

/// One Space: its name, the devices in it, and what can actually be reached.
///
/// # The three things this card has to keep saying
///
/// 1. **A device that has gone stale is listed and marked, never dropped.**
///    Trust ends without anything writing to the Space — a revoke, or a
///    time-limited grant running out — so the engine partitions the members on
///    every read and hands back both halves. Showing a stale device as an
///    ordinary one would be a lie about where a send goes; hiding it would
///    leave someone wondering whether they ever added a device. It is marked,
///    and told what would fix it.
/// 2. **"Nothing can be reached" is not "nothing is in it".** A Space whose
///    every device has gone stale is not an empty Space, and saying "no
///    devices" of one holding three would send the user off to add a device
///    they added months ago. The two states have separate words.
/// 3. **The device id is shown.** It is what the Space actually stores, it is
///    what the CLI prints, and two devices can claim one name — so nobody
///    should have to take a stale row out on the strength of a label a peer
///    chose for itself.
///
/// Nothing here is a permission. Being in a Space grants a device nothing at
/// all: every send is still checked against that device's own permissions, and
/// the copy on this screen says so where someone could otherwise assume
/// otherwise.
class SpaceCard extends StatelessWidget {
  final Space space;

  /// Resolves a device id to something a person recognises, falling back to the
  /// id. Passed in rather than looked up here so the card stays a pure
  /// rendering of one [Space].
  final String Function(String deviceId) nameOf;

  final VoidCallback onRename;
  final VoidCallback onDelete;
  final VoidCallback onAddDevice;

  /// Send files to every member discovery can reach right now.
  ///
  /// Null when the Space has nothing live to send to, so the control is absent
  /// rather than present-and-dead.
  final VoidCallback? onSend;

  /// Take one device out. [stale] is passed through because the two removals
  /// are not equally reversible — see the caller.
  final void Function(String deviceId, {required bool stale}) onRemoveDevice;

  const SpaceCard({
    super.key,
    required this.space,
    required this.nameOf,
    required this.onRename,
    required this.onDelete,
    required this.onAddDevice,
    this.onSend,
    required this.onRemoveDevice,
  });

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Card(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          ListTile(
            key: Key('space-${space.id}'),
            leading: const Icon(Icons.workspaces_outlined),
            title: Text(space.name),
            subtitle: _subtitle(theme),
            isThreeLine: !space.canSend,
            trailing: PopupMenuButton<void>(
              tooltip: 'Space options',
              icon: const Icon(Icons.more_vert_rounded, size: AppIcons.md),
              itemBuilder: (context) => [
                PopupMenuItem<void>(
                  onTap: onRename,
                  child: const ListTile(
                    contentPadding: EdgeInsets.zero,
                    leading: Icon(Icons.edit_outlined),
                    title: Text('Rename'),
                  ),
                ),
                PopupMenuItem<void>(
                  onTap: onDelete,
                  child: const ListTile(
                    contentPadding: EdgeInsets.zero,
                    leading: Icon(Icons.delete_outline_rounded),
                    title: Text('Delete Space'),
                  ),
                ),
              ],
            ),
          ),
          for (final id in space.live) ...[
            const Divider(height: 1),
            _deviceTile(theme, id, stale: false),
          ],
          // Stale devices come last and keep their own marking, rather than
          // being sorted in among the live ones where the eye would skip them.
          for (final id in space.stale) ...[
            const Divider(height: 1),
            _deviceTile(theme, id, stale: true),
          ],
          const Divider(height: 1),
          Padding(
            padding: const EdgeInsets.all(AppSpace.sm),
            // `Wrap`, not a `Row` with a `Spacer`: at 360px the two buttons
            // overflowed by 134px, and a phone is the width this app is most
            // often used at. Wrapping puts Send on its own line instead of
            // clipping it off the screen.
            child: Wrap(
              spacing: AppSpace.sm,
              runSpacing: AppSpace.xxs,
              alignment: WrapAlignment.spaceBetween,
              crossAxisAlignment: WrapCrossAlignment.center,
              children: [
                TextButton.icon(
                  key: Key('space-${space.id}-add'),
                  onPressed: onAddDevice,
                  icon: const Icon(Icons.add_rounded),
                  label: const Text('Add a device'),
                ),
                // Absent, not disabled, when there is nothing live: the
                // summary line above already says why, and a dead button
                // invites a tap that can only disappoint.
                if (onSend != null)
                  FilledButton.tonalIcon(
                    key: Key('space-${space.id}-send'),
                    onPressed: onSend,
                    icon: const Icon(Icons.upload_rounded),
                    label: const Text('Send files'),
                  ),
              ],
            ),
          ),
        ],
      ),
    );
  }

  /// The count, plus the consequence when there is one worth stating.
  Widget _subtitle(ThemeData theme) {
    final unreachable = !space.canSend && space.stale.isNotEmpty;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(summary(space)),
        if (unreachable)
          Text(
            // Requirement of the feature, not decoration: a Space with three
            // revoked devices in it reaches nobody, and "no devices" — the
            // sentence an emptiness check would produce — is both false and
            // actionably misleading.
            'Nothing here can be sent to: every device in this Space is no '
            'longer trusted. Pair with one again, or take it out.',
            style: theme.textTheme.bodySmall?.copyWith(
              color: AppColors.warning(theme.brightness),
            ),
          ),
        if (space.live.isEmpty && space.stale.isEmpty)
          Text(
            'Add a device you already trust.',
            style: theme.textTheme.bodySmall?.copyWith(
              color: theme.colorScheme.onSurfaceVariant,
            ),
          ),
      ],
    );
  }

  Widget _deviceTile(ThemeData theme, String id, {required bool stale}) {
    final name = nameOf(id);
    return ListTile(
      // Keyed by the id, the only field that is unique and the only one the
      // Space actually holds.
      key: Key('space-${space.id}-device-$id'),
      leading: Icon(
        stale ? Icons.report_problem_rounded : Icons.devices_rounded,
        color: stale ? AppColors.warning(theme.brightness) : null,
      ),
      title: Text(name),
      subtitle: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          // Suppressed only when it would repeat the title verbatim, which is
          // what a stale device usually looks like: the trust record that held
          // its name is the one that went away.
          if (name != id) Text(id),
          if (stale)
            Text(
              'No longer trusted, so nothing is sent to it. Pair with it '
              'again, or take it out of this Space.',
              style: theme.textTheme.bodySmall?.copyWith(
                color: AppColors.warning(theme.brightness),
              ),
            ),
        ],
      ),
      isThreeLine: stale,
      trailing: IconButton(
        tooltip: 'Take out of this Space',
        icon: const Icon(Icons.remove_circle_outline_rounded),
        onPressed: () => onRemoveDevice(id, stale: stale),
      ),
    );
  }

  /// How many devices, and how many of them a send would skip.
  ///
  /// Deliberately never "0 devices · 2 no longer trusted": a zero beside a
  /// count reads as an empty Space, which is the one thing this line must not
  /// say about a Space that holds two devices.
  static String summary(Space space) {
    if (space.live.isEmpty && space.stale.isEmpty) return 'No devices yet';
    final live = space.live.length == 1
        ? '1 device'
        : '${space.live.length} devices';
    if (space.stale.isEmpty) return live;
    final stale = space.stale.length == 1
        ? '1 no longer trusted'
        : '${space.stale.length} no longer trusted';
    return space.live.isEmpty ? stale : '$live · $stale';
  }
}
