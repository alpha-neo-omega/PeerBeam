// Reading and writing a group conversation.
//
// # Why this screen had to exist before the feature counted as built
//
// The engine, the FFI, the CLI and this app's repository all carried `invite`,
// `send` and `history` from the start. The Groups screen wired none of them: it
// could create a group, rename it, join one and leave one — and offered no way
// to invite anybody, say anything, or read what had been said. Everything under
// it worked and none of it was reachable, which is the same shape of defect as
// a send that reports success and delivers nothing.
//
// # Group replies, and what they cost
//
// A message here goes to every member this device may message, as N ordinary
// one-to-one sends. There is no group connection and no group key. Members the
// `chat` permission excludes are **named and skipped**, never silently dropped
// — the message did reach the rest, so reporting a failure would be wrong, and
// reporting nothing would be worse.

import 'dart:async';

import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../sdk/events.dart';
import '../../sdk/models.dart';
import '../../platform/chat_notifications.dart' show groupThreadKey;
import '../../state/app_scope.dart';
import '../../state/chat_presence.dart';
import '../../widgets/common.dart';

class GroupChatScreen extends StatefulWidget {
  const GroupChatScreen({
    super.key,
    required this.group,
    required this.nameFor,
  });

  final Group group;

  /// Renders a device id as the name the user knows it by.
  final String Function(String id) nameFor;

  @override
  State<GroupChatScreen> createState() => _GroupChatScreenState();
}

class _GroupChatScreenState extends State<GroupChatScreen> {
  final _composer = TextEditingController();
  final _scroll = ScrollController();
  List<ChatMessage> _messages = const [];
  bool _loading = true;
  bool _sending = false;

  bool _started = false;
  StreamSubscription<BridgeEvent>? _events;

  /// Held from [didChangeDependencies] because `dispose` may not look an
  /// inherited widget up — by then this element is being unmounted.
  ChatPresence? _presence;

  // `didChangeDependencies`, not `initState`: `AppScope.of` establishes an
  // inherited-widget dependency, which Flutter forbids before `initState`
  // completes. Guarded so a later dependency change does not re-fetch.
  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    // Keyed by the group, not by any member — a group message must not be
    // filed under, or silenced by, a private thread with whoever happened to
    // send it.
    //
    // **Only when the scope actually changes.** `didChangeDependencies` fires
    // for any inherited change — a theme switch, a metrics change — including
    // while this route is buried under another transcript. Re-entering there
    // would re-register a screen the user cannot see as the one in front.
    final presence = AppScope.of(context).chatPresence;
    if (!identical(presence, _presence)) {
      _presence?.leave(groupThreadKey(widget.group.id));
      _presence = presence..enter(groupThreadKey(widget.group.id));
    }
    if (_started) return;
    _started = true;
    _load();

    // **Live, not only on open.** A group row is excluded from per-peer
    // history by design, so before the event carried its group there was
    // nowhere an arriving group message could show up at all. Listening here
    // is what makes a reply appear while the thread is on screen.
    final api = AppScope.of(context).api;
    _events = api?.events.listen((e) {
      if (e is ChatReceived && e.message.group == widget.group.id) {
        _load();
      }
    });
  }

  @override
  void dispose() {
    _events?.cancel();
    _composer.dispose();
    _scroll.dispose();
    _presence?.leave(groupThreadKey(widget.group.id));
    super.dispose();
  }

  /// Why the last read of this transcript failed, or null when it came back.
  ///
  /// Kept apart from an empty list on purpose: "Nothing said yet" is a claim
  /// about what the group has written, and a read that failed has no standing
  /// to make it.
  Object? _error;

  Future<void> _load() async {
    final repo = AppScope.of(context).groups;
    final read = await repo.history(widget.group.id);
    if (!mounted) return;
    setState(() {
      // A failed reload keeps what is already on screen: stale messages beat
      // an error page drawn over messages that are right there.
      if (read.error == null || _messages.isEmpty) {
        _messages = read.messages;
      }
      _error = read.error;
      _loading = false;
    });
  }

  void _say(String text) {
    if (!mounted) return;
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(text)));
  }

  Future<void> _send() async {
    final text = _composer.text.trim();
    if (text.isEmpty || _sending) return;
    setState(() => _sending = true);

    final repo = AppScope.of(context).groups;
    final outcome = await repo.send(widget.group.id, text);
    if (!mounted) return;
    setState(() => _sending = false);

    if (outcome.error != null) {
      _say(outcome.error!);
      return;
    }
    // Cleared only once the engine took it, so a failed send leaves the words
    // in the box to try again rather than discarding what was typed.
    _composer.clear();

    final skipped = outcome.result?.skipped ?? const <String>[];
    if (skipped.isNotEmpty) {
      // Named, not counted: "1 skipped" does not tell anyone who missed it.
      _say(
        'Sent — ${skipped.map(widget.nameFor).join(', ')} '
        '${skipped.length == 1 ? 'was' : 'were'} not messaged',
      );
    }
    await _load();
  }

  /// Who this message actually reaches, said the way the group card says it.
  ///
  /// `members` was rendered raw, and that is wrong twice. It includes **this
  /// device**, whose id `nameFor` cannot resolve — so every group header began
  /// with a raw `pb-…` string the user had never seen, presented as a
  /// participant. And it makes a member this device may not message look
  /// exactly like one it can, so the header of the screen where a message is
  /// composed claimed three participants where a send would reach one; the
  /// truth arrived only in the snackbar afterwards, or by going back to the
  /// Groups list, which has marked it correctly all along.
  String _roster() {
    final reachable = widget.group.reachable.map(widget.nameFor);
    final unreachable = widget.group.unreachable.map(
      (id) => '${widget.nameFor(id)} · cannot be messaged',
    );
    final everyone = [...reachable, ...unreachable];
    // `reachable`/`unreachable` already exclude this device (the engine's
    // `recipients(&me)`), so a group of one really does have nobody else in it
    // and should say so rather than rendering an empty line.
    return everyone.isEmpty ? 'Only you' : everyone.join(', ');
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Scaffold(
      appBar: AppBar(
        title: Text(widget.group.name),
        // The roster is the whole cost of a group, so it is on screen rather
        // than a tap away.
        bottom: PreferredSize(
          preferredSize: const Size.fromHeight(20),
          child: Padding(
            padding: const EdgeInsets.only(
              left: AppSpace.md,
              right: AppSpace.md,
              bottom: AppSpace.xs,
            ),
            child: Align(
              alignment: Alignment.centerLeft,
              child: Text(
                _roster(),
                style: theme.textTheme.bodySmall,
                overflow: TextOverflow.ellipsis,
              ),
            ),
          ),
        ),
      ),
      body: Column(
        children: [
          Expanded(
            child: _loading
                ? const Center(child: CircularProgressIndicator())
                : _messages.isEmpty && _error != null
                ? ErrorState(
                    error: _error!,
                    title: 'Could not open this group',
                    onRetry: _load,
                  )
                : _messages.isEmpty
                ? const EmptyState(
                    icon: Icons.forum_outlined,
                    title: 'Nothing said yet',
                    message:
                        'A message here reaches everyone in the group, and '
                        'their replies reach everyone too.',
                  )
                : ListView.builder(
                    controller: _scroll,
                    // Reversed, and indexed from the end, exactly as the
                    // one-to-one thread is (`chat_screen.dart`): index 0 paints
                    // at the BOTTOM, so `_messages.last` — the newest — is
                    // where the viewport opens and stays as messages arrive.
                    //
                    // Without this the group transcript opened on the OLDEST
                    // message and never moved: `_scroll` was constructed, handed
                    // here, and never used, so sending a message scrolled
                    // nothing and the reply appeared off-screen below. The two
                    // transcripts presented opposite ends of a conversation.
                    reverse: true,
                    padding: const EdgeInsets.all(AppSpace.md),
                    itemCount: _messages.length,
                    itemBuilder: (context, i) {
                      final m = _messages[_messages.length - 1 - i];
                      return Align(
                        key: Key('group-message-${m.id}'),
                        alignment: (m.direction == 'out')
                            ? Alignment.centerRight
                            : Alignment.centerLeft,
                        child: Card(
                          color: (m.direction == 'out')
                              ? theme.colorScheme.primaryContainer
                              : null,
                          child: Padding(
                            padding: const EdgeInsets.all(AppSpace.sm),
                            child: Column(
                              crossAxisAlignment: (m.direction == 'out')
                                  ? CrossAxisAlignment.end
                                  : CrossAxisAlignment.start,
                              children: [
                                // Who said it, because in a group "them" is
                                // not enough to know who is talking.
                                if (m.direction != 'out')
                                  Text(
                                    widget.nameFor(m.peerId),
                                    style: theme.textTheme.labelSmall,
                                  ),
                                Text(m.body),
                                // What happened to it. A group send enqueues a
                                // copy per member and returns; with everyone
                                // offline it "succeeds", raises no snackbar,
                                // and used to render exactly like a delivered
                                // message — which it might not be for days.
                                // The one-to-one thread has shown this state
                                // all along.
                                if (m.direction == 'out') ...[
                                  const Gap(AppSpace.xxs),
                                  Text(
                                    _groupStatusLabel(m.status),
                                    style: theme.textTheme.labelSmall?.copyWith(
                                      color: theme.colorScheme.onSurfaceVariant,
                                    ),
                                  ),
                                ],
                              ],
                            ),
                          ),
                        ),
                      );
                    },
                  ),
          ),
          SafeArea(
            top: false,
            child: Padding(
              padding: const EdgeInsets.all(AppSpace.sm),
              child: Row(
                children: [
                  Expanded(
                    child: TextField(
                      key: const Key('group-composer'),
                      controller: _composer,
                      minLines: 1,
                      maxLines: 4,
                      textInputAction: TextInputAction.send,
                      onSubmitted: (_) => _send(),
                      decoration: const InputDecoration(
                        hintText: 'Message everyone',
                        border: OutlineInputBorder(),
                      ),
                    ),
                  ),
                  const Gap(AppSpace.xs),
                  IconButton.filled(
                    key: const Key('group-send'),
                    tooltip: 'Send to everyone',
                    onPressed: _sending ? null : _send,
                    icon: const Icon(Icons.send_rounded),
                  ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// What an outgoing group row's status means to a reader.
///
/// Deliberately cautious about delivery. `group_history` dedupes the copies by
/// id and keeps one arbitrary member's — so the status on the row this screen
/// renders is **one** recipient's, not the group's. Saying "Sent" from that
/// would claim delivery to everybody on the strength of one copy, so the
/// delivered case says what it can actually support.
String _groupStatusLabel(String status) => switch (status) {
  ChatStatusValue.pending => 'Queued',
  ChatStatusValue.staging || ChatStatusValue.transferring => 'Sending…',
  ChatStatusValue.failed => 'Failed',
  ChatStatusValue.declined => 'Declined',
  ChatStatusValue.interrupted => 'Interrupted',
  _ => 'Sent',
};
