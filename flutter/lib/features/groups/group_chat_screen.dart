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

import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../sdk/models.dart';
import '../../state/app_scope.dart';
import '../../widgets/common.dart';

class GroupChatScreen extends StatefulWidget {
  const GroupChatScreen({super.key, required this.group, required this.nameFor});

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

  // `didChangeDependencies`, not `initState`: `AppScope.of` establishes an
  // inherited-widget dependency, which Flutter forbids before `initState`
  // completes. Guarded so a later dependency change does not re-fetch.
  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    if (_started) return;
    _started = true;
    _load();
  }

  @override
  void dispose() {
    _composer.dispose();
    _scroll.dispose();
    super.dispose();
  }

  Future<void> _load() async {
    final repo = AppScope.of(context).groups;
    final messages = await repo.history(widget.group.id);
    if (!mounted) return;
    setState(() {
      _messages = messages;
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
                widget.group.members.map(widget.nameFor).join(', '),
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
                    padding: const EdgeInsets.all(AppSpace.md),
                    itemCount: _messages.length,
                    itemBuilder: (context, i) {
                      final m = _messages[i];
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
