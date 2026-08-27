import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../sdk/models.dart';
import '../../state/app_scope.dart';
import '../../widgets/common.dart';
import 'join_dialog.dart';

/// Groups: conversations a set of devices all share.
///
/// # What this screen must keep saying
///
/// 1. **A Group is not a Space.** A Space is private to this device and nobody
///    learns who is in it; a Group is the opposite trade. Both exist, neither
///    replaces the other, and a user who confuses them will disclose something
///    they did not mean to. The empty state says which is which rather than
///    assuming the distinction is obvious.
/// 2. **An invitation is not a membership.** Offers appear in their own
///    section, above the groups, and nothing about holding one changes what
///    this device is in. Mixing them would show a group the user has not
///    joined.
/// 3. **A member that cannot be messaged is named.** The engine reports
///    `reachable` and `unreachable` separately, and this shows both — a list
///    that quietly shrank would leave somebody wondering whether they had ever
///    added a device.
///
/// # Why declining says nothing to the inviter
///
/// Turning an offer down is local and silent. Telling the inviter would publish
/// "this device saw your invitation and refused", which is a fact about the
/// user nobody asked to share; ignoring an offer is allowed to look exactly
/// like never having seen it. The button copy says so, so nobody declines while
/// worrying about a message that is never sent.
class GroupsScreen extends StatefulWidget {
  const GroupsScreen({super.key});

  @override
  State<GroupsScreen> createState() => _GroupsScreenState();
}

class _GroupsScreenState extends State<GroupsScreen> {
  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) AppScope.of(context).groups.refresh();
    });
  }

  void _say(String message) {
    if (!mounted) return;
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(message)));
  }

  /// The name this app knows a device by, or its id when it does not know one.
  String _nameFor(String id) {
    final state = AppScope.of(context);
    for (final d in state.device.devices) {
      if (d.id == id) return d.name;
    }
    for (final t in state.trust.items) {
      if (t.id == id && t.name.isNotEmpty) return t.name;
    }
    return id;
  }

  /// Peer targets for the members the app can currently reach.
  ///
  /// The engine holds ids; routes come from discovery, which is here. A member
  /// this app cannot see is simply not in the list — they are told at next
  /// contact rather than blocking the operation.
  List<PeerTarget> _reachable(List<String> ids) {
    // `peerTarget` is the one place that knows a discovered device is only
    // addressable when it has an address and a port — the same precondition the
    // engine's `device_from` enforces. Rebuilding a target here would be a
    // second copy of that rule, and the copy would be the one that got it
    // wrong.
    final devices = AppScope.of(context).device;
    return [for (final id in ids) ?devices.peerTarget(id)];
  }

  Future<void> _create() async {
    final name = await showDialog<String>(
      context: context,
      builder: (dialogContext) => _NameDialog(
        title: 'New group',
        hint: 'What is it for?',
        confirm: 'Create',
      ),
    );
    if (name == null || !mounted) return;
    final error = await AppScope.of(context).groups.create(name);
    if (error != null) _say(error);
  }

  Future<void> _rename(Group group) async {
    final name = await showDialog<String>(
      context: context,
      builder: (dialogContext) => _NameDialog(
        title: 'Rename ${group.name}',
        hint: 'New name',
        confirm: 'Rename',
        initial: group.name,
      ),
    );
    if (name == null || !mounted) return;
    final error = await AppScope.of(context).groups.rename(group.id, name);
    _say(error ?? 'Renamed on this device only — names are not shared');
  }

  Future<void> _join(GroupInvite invite) async {
    final agreed = await askToJoinGroup(context, invite, nameFor: _nameFor);
    if (!agreed || !mounted) return;
    final state = AppScope.of(context);
    final peers = _reachable(invite.members);
    final error = await state.groups.accept(invite.group, peers);
    if (error != null) {
      _say(error);
      return;
    }
    final missed = invite.members.length - peers.length;
    _say(
      missed == 0
          ? 'Joined ${invite.name}'
          : 'Joined ${invite.name} — $missed member(s) will hear when they are reachable',
    );
  }

  Future<void> _decline(GroupInvite invite) async {
    final error = await AppScope.of(context).groups.decline(invite.group);
    _say(error ?? 'Declined — ${_nameFor(invite.from)} is not told');
  }

  Future<void> _leave(Group group) async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (dialogContext) => AlertDialog(
        icon: const Icon(Icons.logout_rounded),
        title: Text('Leave ${group.name}?'),
        content: const Text(
          'The members are told, and this device forgets the group. Anyone who '
          'does not get the message may keep sending to you — withhold '
          'Messages from that device to refuse it.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(dialogContext).pop(false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(dialogContext).pop(true),
            child: const Text('Leave'),
          ),
        ],
      ),
    );
    if (confirmed != true || !mounted) return;
    final error = await AppScope.of(
      context,
    ).groups.leave(group.id, _reachable(group.reachable));
    if (error != null) _say(error);
  }

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    return Scaffold(
      appBar: AppBar(title: const Text('Groups')),
      floatingActionButton: FloatingActionButton.extended(
        onPressed: _create,
        icon: const Icon(Icons.add_rounded),
        label: const Text('New group'),
      ),
      body: SafeArea(
        child: ContentPane(
          child: AnimatedBuilder(
            animation: state.groups,
            builder: (context, _) {
              final repo = state.groups;
              if (!repo.loaded) {
                return const Center(child: CircularProgressIndicator());
              }
              // A read that failed is not "you are in no groups" — saying so
              // would be a statement about the user's own memberships that a
              // failure has no grounds to make.
              if (repo.error != null && repo.groups.isEmpty) {
                return ErrorState(
                  error: repo.error!,
                  title: 'Could not read your groups',
                  onRetry: repo.refresh,
                );
              }
              if (repo.groups.isEmpty && repo.invites.isEmpty) {
                return const EmptyState(
                  icon: Icons.groups_outlined,
                  title: 'No groups yet',
                  message:
                      'A group is a conversation everyone in it can reply to — '
                      'and everyone in it sees who else is there. For sending '
                      'to several devices without that, use a Space instead.',
                );
              }
              return ListView(
                padding: const EdgeInsets.all(AppSpace.md),
                children: [
                  if (repo.invites.isNotEmpty) ...[
                    Text(
                      'Invitations',
                      style: Theme.of(context).textTheme.labelLarge,
                    ),
                    const Gap(AppSpace.xs),
                    for (final invite in repo.invites)
                      Card(
                        key: Key('invite-${invite.group}'),
                        child: ListTile(
                          leading: const Icon(Icons.mark_email_unread_outlined),
                          title: Text(invite.name),
                          subtitle: Text(
                            '${_nameFor(invite.from)} invited you · '
                            '${invite.members.length} device(s) inside',
                          ),
                          isThreeLine: true,
                          trailing: Wrap(
                            spacing: AppSpace.xxs,
                            children: [
                              // "Ignore", not "Decline": nothing is sent, and
                              // a word implying a reply would misdescribe what
                              // the button does. It also keeps this distinct
                              // from the dialog's own dismiss action, which is
                              // a different decision on a different screen.
                              TextButton(
                                onPressed: () => _decline(invite),
                                child: const Text('Ignore'),
                              ),
                              FilledButton.tonal(
                                onPressed: () => _join(invite),
                                child: const Text('Review'),
                              ),
                            ],
                          ),
                        ),
                      ),
                    const Gap(AppSpace.md),
                  ],
                  if (repo.groups.isNotEmpty)
                    Text(
                      'Your groups',
                      style: Theme.of(context).textTheme.labelLarge,
                    ),
                  const Gap(AppSpace.xs),
                  for (final group in repo.groups)
                    _GroupCard(
                      group: group,
                      nameFor: _nameFor,
                      onRename: () => _rename(group),
                      onLeave: () => _leave(group),
                    ),
                ],
              );
            },
          ),
        ),
      ),
    );
  }
}

class _GroupCard extends StatelessWidget {
  const _GroupCard({
    required this.group,
    required this.nameFor,
    required this.onRename,
    required this.onLeave,
  });

  final Group group;
  final String Function(String id) nameFor;
  final VoidCallback onRename;
  final VoidCallback onLeave;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Card(
      key: Key('group-${group.id}'),
      child: Padding(
        padding: const EdgeInsets.all(AppSpace.md),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Expanded(
                  child: Text(group.name, style: theme.textTheme.titleMedium),
                ),
                IconButton(
                  tooltip: 'Rename',
                  icon: const Icon(Icons.edit_outlined),
                  onPressed: onRename,
                ),
                IconButton(
                  tooltip: 'Leave',
                  icon: const Icon(Icons.logout_rounded),
                  onPressed: onLeave,
                ),
              ],
            ),
            for (final id in group.reachable)
              Padding(
                padding: const EdgeInsets.only(top: AppSpace.xxs),
                child: Text(nameFor(id), style: theme.textTheme.bodySmall),
              ),
            // Named rather than hidden: a member this device may not message is
            // still in the group, and a shorter list would read as if they had
            // never been added.
            for (final id in group.unreachable)
              Padding(
                padding: const EdgeInsets.only(top: AppSpace.xxs),
                child: Text(
                  '${nameFor(id)} · cannot be messaged',
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: theme.colorScheme.onSurfaceVariant,
                  ),
                ),
              ),
          ],
        ),
      ),
    );
  }
}

/// A one-field dialog for a group name.
class _NameDialog extends StatefulWidget {
  const _NameDialog({
    required this.title,
    required this.hint,
    required this.confirm,
    this.initial,
  });

  final String title;
  final String hint;
  final String confirm;
  final String? initial;

  @override
  State<_NameDialog> createState() => _NameDialogState();
}

class _NameDialogState extends State<_NameDialog> {
  late final TextEditingController _controller = TextEditingController(
    text: widget.initial ?? '',
  );

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  void _submit() {
    final name = _controller.text.trim();
    if (name.isEmpty) return;
    Navigator.of(context).pop(name);
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: Text(widget.title),
    content: TextField(
      controller: _controller,
      autofocus: true,
      decoration: InputDecoration(hintText: widget.hint),
      // Enter submits, so the keyboard is not a dead end on a phone.
      onSubmitted: (_) => _submit(),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.of(context).pop(),
        child: const Text('Cancel'),
      ),
      FilledButton(onPressed: _submit, child: Text(widget.confirm)),
    ],
  );
}
