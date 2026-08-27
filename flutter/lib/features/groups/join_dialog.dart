import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../sdk/models.dart';

/// The dialog shown before joining a group, and the one place in the app where
/// amendment **A2's fifth condition** is actually kept.
///
/// # What this has to say, and why it is not a footnote
///
/// A Space discloses nothing: no member learns who else is in one. A Group is
/// the opposite trade, and the trade is the whole feature — every member learns
/// every other member, the moment you accept, permanently. That cannot be
/// withdrawn afterwards: the roster has already reached their devices, and
/// there is no authority to recall it from.
///
/// So the names are **shown**, not summarised as a count. "3 people" is a fact
/// about arithmetic; the list is the fact the user is actually agreeing to.
/// Somebody who would decline because of one specific device in the room cannot
/// act on a number.
///
/// # Why the confirm button says what it does
///
/// Not "OK", and not "Accept". The button names the consequence — *Join and
/// share who I am* — because a person clicking through a dialog reads the
/// button far more often than the paragraph, and a button that says "OK" has
/// told them nothing about what they just agreed to.
class JoinGroupDialog extends StatelessWidget {
  const JoinGroupDialog({super.key, required this.invite, this.nameFor});

  final GroupInvite invite;

  /// Turns a device id into the name the user knows it by, when the app has
  /// one. Ids are shown as-is otherwise — an unfamiliar id is still a truthful
  /// answer, and inventing a friendly label for a device this app has never
  /// seen would be worse.
  final String Function(String id)? nameFor;

  String _label(String id) => nameFor?.call(id) ?? id;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final from = _label(invite.from);
    // Everyone who will learn this device: the existing members, and the
    // inviter if they are somehow not among them.
    final learners = <String>{invite.from, ...invite.members}.toList();

    return AlertDialog(
      icon: const Icon(Icons.groups_rounded),
      title: Text('Join ${invite.name}?'),
      content: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('$from invited you.'),
            const Gap(AppSpace.md),
            // Stated before the list, so somebody who reads one line reads
            // this one.
            Text(
              'Everyone in this group will see your device, and you will see '
              'theirs. That cannot be undone later.',
              style: theme.textTheme.bodyMedium?.copyWith(
                fontWeight: FontWeight.w600,
              ),
            ),
            const Gap(AppSpace.md),
            Text(
              learners.length == 1
                  ? 'One device is in it:'
                  : '${learners.length} devices are in it:',
              style: theme.textTheme.labelLarge?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
            ),
            const Gap(AppSpace.xs),
            // Named, not counted — see the class doc.
            for (final id in learners)
              Padding(
                padding: const EdgeInsets.only(bottom: AppSpace.xxs),
                child: Row(
                  children: [
                    Icon(
                      Icons.devices_other_rounded,
                      size: AppIcons.sm,
                      color: theme.colorScheme.onSurfaceVariant,
                    ),
                    const Gap(AppSpace.xs),
                    Expanded(
                      child: Text(_label(id), style: theme.textTheme.bodySmall),
                    ),
                  ],
                ),
              ),
            const Gap(AppSpace.md),
            Text(
              'Messages you send here go to each of them separately. Nothing '
              'passes through a server.',
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
            ),
          ],
        ),
      ),
      actions: [
        // Declining is silent: the inviter is not told. Saying so here stops a
        // user declining out of worry about a message that is never sent.
        TextButton(
          onPressed: () => Navigator.of(context).pop(false),
          child: const Text('Not now'),
        ),
        FilledButton(
          onPressed: () => Navigator.of(context).pop(true),
          child: const Text('Join and share who I am'),
        ),
      ],
    );
  }
}

/// Ask whether to join, and report the answer.
///
/// Returns true **only** on the explicit confirm. Cancel, the back button, a
/// tap outside and a route popped from under it all return false: `showDialog`
/// completes with null when dismissed, and null is not consent — the same bar
/// the pairing prompt sets, and for a decision with the same one-way cost.
Future<bool> askToJoinGroup(
  BuildContext context,
  GroupInvite invite, {
  String Function(String id)? nameFor,
}) async {
  final joined = await showDialog<bool>(
    context: context,
    builder: (_) => JoinGroupDialog(invite: invite, nameFor: nameFor),
  );
  return joined ?? false;
}
