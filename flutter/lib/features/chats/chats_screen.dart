import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../sdk/error_text.dart';
import '../../sdk/models.dart' show ChatConversation, PeerTarget;
import '../../state/app_scope.dart';
import '../../state/models.dart';
import '../../widgets/appear.dart';
import '../../widgets/common.dart';
import '../chat/chat_screen.dart';

/// Chats — every conversation this device holds, whether or not discovery can
/// currently see the peer.
///
/// It is derived from what is on disk rather than from the network, which is
/// the whole point: a peer that has gone offline has no device tile, so without
/// this its thread — and the files queued inside it — would have no entry point
/// at all. Listens to the chat store only (plus discovery and trust, which are
/// only ever consulted for a *name*), so transfers and history never rebuild
/// it.
class ChatsScreen extends StatefulWidget {
  const ChatsScreen({super.key});

  @override
  State<ChatsScreen> createState() => _ChatsScreenState();
}

class _ChatsScreenState extends State<ChatsScreen> {
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

  /// The best name we can put to a conversation's peer id.
  ///
  /// Discovery first (a live name, kept current), then the trust store (a peer
  /// we have chatted with has almost always been pinned, and that name outlives
  /// discovery), and finally the device id itself — ugly, but the truth, and
  /// never a fabricated "Unknown device" that two different peers would share.
  String _peerName(String peerId) {
    final state = AppScope.of(context);
    for (final d in state.device.devices) {
      if (d.id == peerId) return d.name;
    }
    for (final t in state.trust.items) {
      if (t.id == peerId && t.name.isNotEmpty) return t.name;
    }
    return peerId;
  }

  /// Open a thread.
  ///
  /// Keyed by the **authenticated peer id the engine returned**, never a
  /// locally minted one: that is the whole reason this list can reach a peer
  /// discovery cannot see.
  ///
  /// The send target is discovery's when it has one and an address-less
  /// placeholder otherwise — the thread stays readable either way, and the chat
  /// screen disables its composer rather than accepting messages the engine
  /// would refuse.
  void _open(String peerId) {
    final state = AppScope.of(context);
    final target =
        state.device.peerTarget(peerId) ??
        PeerTarget(
          id: peerId,
          name: _peerName(peerId),
          addresses: const [],
          port: 0,
        );
    Navigator.of(context).push(
      MaterialPageRoute(
        builder: (_) => ChatScreen(peerId: peerId, peer: target),
      ),
    );
  }

  /// Confirm, then delete this device's copy of a thread.
  ///
  /// The confirmation states two things and no more, because two things are all
  /// this can honestly promise before the fact: the thread is removed **from
  /// this device**, and anything still waiting to be sent is kept and still
  /// sent. It deliberately quotes no counts — the number of records removed and
  /// the number kept are decided by the engine as it deletes (what is queued
  /// can change up to that moment), so they are reported afterwards, from the
  /// engine's own answer, rather than guessed at here beforehand.
  Future<void> _confirmDelete(ChatConversation c) async {
    final name = _peerName(c.peerId);
    final messenger = ScaffoldMessenger.of(context);
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: Text('Delete "$name"?'),
        content: const Text(
          'This removes the conversation from this device. Anything still '
          'waiting to be sent is kept and will still be sent.\n\n'
          'The other device keeps its own copy — nothing is deleted there.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(ctx, true),
            child: const Text('Delete'),
          ),
        ],
      ),
    );
    if (confirmed != true || !mounted) return;

    void snack(String m) => messenger
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(m)));
    try {
      final r = await AppScope.of(context).chat.deleteConversation(c.peerId);
      snack(_outcome(name, r));
    } catch (e) {
      // Reported, never swallowed: a refusal here is the engine declining to
      // delete because it could not establish what is still queued, and the
      // thread is still there. Saying nothing would look like it worked.
      snack('Could not delete "$name" — ${friendlyError(e)}');
    }
  }

  /// What actually happened, in the engine's own numbers.
  ///
  /// Both halves are counts the engine returned, never an assumption: `kept`
  /// says how many records were deliberately left because they still back a
  /// queued message, which is why the thread may still be listed afterwards.
  /// Saying "and will still be sent" is the promise the confirmation made, and
  /// it is only made when there is genuinely something to send.
  static String _outcome(String name, ({int removed, int kept}) r) {
    final removed = r.removed == 1
        ? 'Deleted 1 message from "$name"'
        : 'Deleted ${r.removed} messages from "$name"';
    if (r.kept == 0) return removed;
    final kept = r.kept == 1
        ? '1 queued message was kept and will still be sent'
        : '${r.kept} queued messages were kept and will still be sent';
    return '$removed · $kept';
  }

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    return Scaffold(
      appBar: AppBar(title: const Text('Chats')),
      body: SafeArea(
        child: ContentPane(
          child: AnimatedBuilder(
            // Discovery and trust are listened to as well, because they supply
            // the *name* a row renders: a peer coming online must relabel its
            // thread without the user leaving and coming back.
            animation: Listenable.merge([
              state.chat,
              state.device,
              state.trust,
            ]),
            builder: (context, _) {
              final conversations = state.chat.conversations;
              if (conversations.isEmpty) {
                return const EmptyState(
                  icon: Icons.forum_outlined,
                  title: 'No conversations yet',
                  message:
                      'Start one from a device on the Home screen. Threads stay '
                      'here even when the other device goes offline.',
                );
              }
              return ListView.builder(
                padding: const EdgeInsets.all(AppSpace.md),
                itemCount: conversations.length,
                itemBuilder: (context, i) => Appear(
                  index: i,
                  child: Padding(
                    padding: const EdgeInsets.only(bottom: AppSpace.xs),
                    child: ConversationCard(
                      conversation: conversations[i],
                      name: _peerName(conversations[i].peerId),
                      onTap: () => _open(conversations[i].peerId),
                      onDelete: () => _confirmDelete(conversations[i]),
                    ),
                  ),
                ),
              );
            },
          ),
        ),
      ),
    );
  }
}

/// One conversation row: who it is with, what it is waiting on, and an overflow
/// menu for the one destructive action there is.
class ConversationCard extends StatelessWidget {
  final ChatConversation conversation;

  /// The peer's display name, already resolved by the caller (discovery, then
  /// the trust store, then the device id itself).
  final String name;
  final VoidCallback onTap;
  final VoidCallback onDelete;

  const ConversationCard({
    super.key,
    required this.conversation,
    required this.name,
    required this.onTap,
    required this.onDelete,
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
              PopupMenuButton<void>(
                tooltip: 'Conversation options',
                icon: const Icon(Icons.more_vert_rounded, size: AppIcons.md),
                itemBuilder: (context) => [
                  PopupMenuItem<void>(
                    onTap: onDelete,
                    child: const ListTile(
                      contentPadding: EdgeInsets.zero,
                      leading: Icon(Icons.delete_outline_rounded),
                      title: Text('Delete conversation'),
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
