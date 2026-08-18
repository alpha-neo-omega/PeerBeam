import 'dart:async';

import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../sdk/error_text.dart';
import '../../sdk/models.dart'
    show ChatConversation, ChatSearchHit, ChatSearchResults, PeerTarget;
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
  /// How long the field stays quiet before a query is run.
  ///
  /// Every keystroke would otherwise be a full walk of every conversation in
  /// the engine. Short enough to feel immediate, long enough that typing a word
  /// costs one search rather than one per letter.
  static const _debounce = Duration(milliseconds: 250);

  final _search = TextEditingController();
  Timer? _timer;

  /// The query the visible results belong to. Empty means the conversation
  /// list is showing, not an empty search.
  String _query = '';

  /// Null until a search has answered for the current query.
  ChatSearchResults? _results;
  bool _searching = false;

  /// Which search the results on screen came from.
  ///
  /// Searches are dispatched per keystroke-burst and answered out of order:
  /// a broad query started first can easily land after the narrower one typed
  /// on top of it, and applying it would show the user results for a query they
  /// have already replaced. Every dispatch takes the next number and only the
  /// newest may write to the screen.
  int _seq = 0;

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

  @override
  void dispose() {
    _timer?.cancel();
    _search.dispose();
    super.dispose();
  }

  /// Debounce, then search. An emptied field returns to the conversation list
  /// immediately — with any in-flight search stranded, so its answer cannot
  /// arrive afterwards and repopulate a search the user has just cleared.
  void _onQueryChanged(String raw) {
    _timer?.cancel();
    final query = raw.trim();
    if (query.isEmpty) {
      _seq++;
      setState(() {
        _query = '';
        _results = null;
        _searching = false;
      });
      return;
    }
    setState(() {
      _query = query;
      _searching = true;
    });
    _timer = Timer(_debounce, () => _run(query));
  }

  Future<void> _run(String query) async {
    final chat = AppScope.of(context).chat;
    final seq = ++_seq;
    final results = await chat.search(query);
    // Stale: the user has typed again, or cleared the field, since this went
    // out.
    if (!mounted || seq != _seq) return;
    setState(() {
      _results = results;
      _searching = false;
    });
  }

  /// Group hits by conversation, keeping the engine's newest-first order: the
  /// threads appear in the order their newest hit did, and the hits inside each
  /// stay as they came.
  static List<({String peerId, List<ChatSearchHit> hits})> _grouped(
    List<ChatSearchHit> hits,
  ) {
    final order = <String>[];
    final byPeer = <String, List<ChatSearchHit>>{};
    for (final hit in hits) {
      byPeer.putIfAbsent(hit.peerId, () {
        order.add(hit.peerId);
        return <ChatSearchHit>[];
      }).add(hit);
    }
    return [for (final peerId in order) (peerId: peerId, hits: byPeer[peerId]!)];
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
              return Column(
                children: [
                  // Offered even with no conversations yet: a field that
                  // appears only once there is something to find is a field
                  // nobody knows is there.
                  Padding(
                    padding: const EdgeInsets.fromLTRB(
                      AppSpace.md,
                      AppSpace.sm,
                      AppSpace.md,
                      AppSpace.xs,
                    ),
                    child: _SearchField(
                      controller: _search,
                      onChanged: _onQueryChanged,
                      onClear: () {
                        _search.clear();
                        _onQueryChanged('');
                      },
                    ),
                  ),
                  Expanded(
                    child: _query.isEmpty
                        ? _conversationList(conversations)
                        : _resultList(),
                  ),
                ],
              );
            },
          ),
        ),
      ),
    );
  }

  Widget _conversationList(List<ChatConversation> conversations) {
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
  }

  /// Search results, grouped by conversation.
  ///
  /// The truncation notice sits at the **top**, above the first hit, and not at
  /// the end of the list. It is the one thing the user has to read before
  /// concluding that what they were looking for is not there, and a footer
  /// under fifty rows is read after that conclusion has already been drawn.
  Widget _resultList() {
    final results = _results;
    if (results == null) {
      // Nothing has answered for this query yet.
      return const Center(child: CircularProgressIndicator());
    }
    if (results.isEmpty) {
      // Never "nothing matches" while a newer search is still out: the answer
      // on screen belongs to a query the user has already replaced, and saying
      // it does not exist is the one wrong thing a search can say.
      if (_searching) return const Center(child: CircularProgressIndicator());
      return EmptyState(
        icon: Icons.search_off_rounded,
        title: 'No messages match "$_query"',
        message:
            'Searches this device\'s own conversations — the text of messages '
            'and the names of shared files.',
      );
    }
    final rows = <_ResultRow>[
      if (results.truncated) _TruncatedRow(results.limit),
      for (final group in _grouped(results.hits)) ...[
        _PeerRow(group.peerId, group.hits.length),
        for (final hit in group.hits) _HitRow(hit),
      ],
    ];
    return Column(
      children: [
        // Results for the previous query stay readable while a newer one runs,
        // with this to say so — a list that blanked on every keystroke would
        // flash more than it informed.
        if (_searching) const LinearProgressIndicator(minHeight: 2),
        Expanded(child: _rowList(rows)),
      ],
    );
  }

  Widget _rowList(List<_ResultRow> rows) {
    return ListView.builder(
      padding: const EdgeInsets.fromLTRB(
        AppSpace.md,
        0,
        AppSpace.md,
        AppSpace.md,
      ),
      itemCount: rows.length,
      itemBuilder: (context, i) => switch (rows[i]) {
        _TruncatedRow(:final limit) => _TruncationNotice(limit: limit),
        _PeerRow(:final peerId, :final count) => SectionHeader(
          title: _peerName(peerId),
          trailing: Text(
            count == 1 ? '1 match' : '$count matches',
            style: Theme.of(context).textTheme.labelMedium?.copyWith(
              color: Theme.of(context).colorScheme.onSurfaceVariant,
            ),
          ),
        ),
        _HitRow(:final hit) => Padding(
          padding: const EdgeInsets.only(bottom: AppSpace.xs),
          child: SearchHitCard(hit: hit, onTap: () => _open(hit.peerId)),
        ),
      },
    );
  }
}

/// One row of the results list.
sealed class _ResultRow {
  const _ResultRow();
}

/// "There were more matches than fitted" — first in the list, never last.
class _TruncatedRow extends _ResultRow {
  final int limit;
  const _TruncatedRow(this.limit);
}

/// The conversation the hits below it are in.
class _PeerRow extends _ResultRow {
  final String peerId;
  final int count;
  const _PeerRow(this.peerId, this.count);
}

class _HitRow extends _ResultRow {
  final ChatSearchHit hit;
  const _HitRow(this.hit);
}

/// The query field. Its clear button is deliberately always reachable once
/// there is anything to clear — returning to the conversation list must not
/// require selecting and deleting text.
class _SearchField extends StatelessWidget {
  final TextEditingController controller;
  final ValueChanged<String> onChanged;
  final VoidCallback onClear;

  const _SearchField({
    required this.controller,
    required this.onChanged,
    required this.onClear,
  });

  @override
  Widget build(BuildContext context) {
    return TextField(
      controller: controller,
      onChanged: onChanged,
      textInputAction: TextInputAction.search,
      decoration: InputDecoration(
        isDense: true,
        hintText: 'Search messages and file names',
        prefixIcon: const Icon(Icons.search_rounded, size: AppIcons.md),
        suffixIcon: controller.text.isEmpty
            ? null
            : IconButton(
                tooltip: 'Clear search',
                icon: const Icon(Icons.close_rounded, size: AppIcons.md),
                onPressed: onClear,
              ),
        border: const OutlineInputBorder(),
      ),
    );
  }
}

/// The notice that says the result set was cut.
///
/// Shown whenever the engine reports truncation, with no threshold and no way
/// to dismiss it: a bounded search whose bound is invisible reads as "that is
/// all there is", and for a search over your own history that is a wrong answer
/// rather than a partial one.
class _TruncationNotice extends StatelessWidget {
  final int limit;
  const _TruncationNotice({required this.limit});

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Padding(
      padding: const EdgeInsets.only(top: AppSpace.xs, bottom: AppSpace.xs),
      child: Container(
        padding: const EdgeInsets.all(AppSpace.sm),
        decoration: BoxDecoration(
          color: scheme.tertiaryContainer,
          borderRadius: BorderRadius.circular(AppRadius.md),
        ),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Icon(
              Icons.filter_list_rounded,
              size: AppIcons.md,
              color: scheme.onTertiaryContainer,
            ),
            const Gap(AppSpace.sm),
            Expanded(
              child: Text(
                'Showing the newest $limit matches — there are more. '
                'Narrow your search to see the rest.',
                style: Theme.of(context).textTheme.bodySmall?.copyWith(
                  color: scheme.onTertiaryContainer,
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// One matching message.
///
/// Tapping it opens the conversation the message is in — **not** the message
/// itself. The thread is a lazily-built, variable-height reversed list with no
/// scroll controller, so scrolling to an arbitrary row means either a
/// positioned-list dependency or measuring every bubble above it; neither is a
/// small change, and half of one would be a jump that lands in the wrong place.
/// See `docs/UI.md`.
class SearchHitCard extends StatelessWidget {
  final ChatSearchHit hit;
  final VoidCallback onTap;

  const SearchHitCard({super.key, required this.hit, required this.onTap});

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    final at = hit.at;
    return Card(
      margin: EdgeInsets.zero,
      child: InkWell(
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.symmetric(
            horizontal: AppSpace.sm,
            vertical: AppSpace.xs,
          ),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Icon(
                hit.isFile
                    ? Icons.insert_drive_file_outlined
                    : Icons.chat_bubble_outline_rounded,
                size: AppIcons.md,
                color: scheme.onSurfaceVariant,
              ),
              const Gap(AppSpace.sm),
              Expanded(
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    // The snippet exactly as the engine cut it — a substring of
                    // what is stored, not a re-rendering of it.
                    Text(
                      hit.snippet,
                      maxLines: 2,
                      overflow: TextOverflow.ellipsis,
                      style: text.bodyMedium,
                    ),
                    const Gap(2),
                    Text(
                      switch ((hit.isMine, at)) {
                        (true, final DateTime t) => 'You · ${formatAgo(t)}',
                        (false, final DateTime t) => 'Them · ${formatAgo(t)}',
                        (true, _) => 'You',
                        (false, _) => 'Them',
                      },
                      style: text.bodySmall?.copyWith(
                        color: scheme.onSurfaceVariant,
                      ),
                    ),
                  ],
                ),
              ),
            ],
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
