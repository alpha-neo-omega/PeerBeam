import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../sdk/models.dart';
import '../../state/app_scope.dart';
import '../../widgets/appear.dart';
import '../../widgets/common.dart';

/// A one-to-one chat with [peer]. [peerId] is the discovered device's real
/// id — the conversation key. [PeerTarget] does carry its own `id` field
/// now, but it's optional (a manually-entered host:port target has none), so
/// the id is threaded through separately here rather than read off [peer].
class ChatScreen extends StatefulWidget {
  final String peerId;
  final PeerTarget peer;
  const ChatScreen({super.key, required this.peerId, required this.peer});

  @override
  State<ChatScreen> createState() => _ChatScreenState();
}

class _ChatScreenState extends State<ChatScreen> {
  final _controller = TextEditingController();

  @override
  void initState() {
    super.initState();
    // Refresh once the first frame is up (not in initState directly — the
    // repository may not be attached to a live engine yet during boot, same
    // reasoning as the other repositories' "don't refresh in the constructor"
    // note).
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      AppScope.of(context).chat.refresh(widget.peerId);
    });
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  /// Fire-and-forget: `send` awaits a synchronous dial+handshake under the
  /// hood, so the button handler must not block on it — the optimistic
  /// message (appended inside the repository, before its own await) is what
  /// keeps the tap responsive.
  void _send() {
    final text = _controller.text;
    if (text.trim().isEmpty) return;
    final chat = AppScope.of(context).chat;
    _controller.clear();
    chat.send(widget.peerId, widget.peer, text);
  }

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    return Scaffold(
      appBar: AppBar(title: Text(widget.peer.name)),
      body: SafeArea(
        child: ContentPane(
          child: Column(
            children: [
              Expanded(
                child: AnimatedBuilder(
                  animation: state.chat,
                  builder: (context, _) {
                    final items = state.chat.messagesFor(widget.peerId);
                    if (items.isEmpty) {
                      return const EmptyState(
                        icon: Icons.chat_bubble_outline_rounded,
                        title: 'No messages yet',
                        message: 'Send a message to start the conversation.',
                      );
                    }
                    // Reversed so the latest message stays pinned to the
                    // bottom without a manual scroll controller.
                    return ListView.builder(
                      reverse: true,
                      padding: const EdgeInsets.all(AppSpace.md),
                      itemCount: items.length,
                      itemBuilder: (context, i) => Appear(
                        index: i,
                        child: _ChatBubble(message: items[items.length - 1 - i]),
                      ),
                    );
                  },
                ),
              ),
              _Composer(controller: _controller, onSend: _send),
            ],
          ),
        ),
      ),
    );
  }
}

/// One message bubble: own messages lean right in `primaryContainer`, the
/// peer's lean left in `surfaceContainerHighest`.
class _ChatBubble extends StatelessWidget {
  final ChatMessage message;
  const _ChatBubble({required this.message});

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    final mine = message.isMine;
    final bg = mine ? scheme.primaryContainer : scheme.surfaceContainerHighest;
    final fg = mine ? scheme.onPrimaryContainer : scheme.onSurface;

    return Padding(
      padding: const EdgeInsets.only(bottom: AppSpace.xs),
      child: Row(
        mainAxisAlignment: mine
            ? MainAxisAlignment.end
            : MainAxisAlignment.start,
        children: [
          ConstrainedBox(
            constraints: BoxConstraints(
              maxWidth: MediaQuery.sizeOf(context).width * 0.75,
            ),
            child: Container(
              padding: const EdgeInsets.symmetric(
                horizontal: AppSpace.sm,
                vertical: AppSpace.xs,
              ),
              decoration: BoxDecoration(
                color: bg,
                borderRadius: BorderRadius.circular(AppRadius.lg),
              ),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                mainAxisSize: MainAxisSize.min,
                children: [
                  Text(message.body, style: text.bodyMedium?.copyWith(color: fg)),
                  const Gap(AppSpace.xxs),
                  Text(
                    _time(message.at),
                    style: text.labelSmall?.copyWith(
                      color: fg.withValues(alpha: 0.7),
                    ),
                  ),
                ],
              ),
            ),
          ),
        ],
      ),
    );
  }

  // The engine persists timestamps as UTC (RFC3339); the optimistic message
  // is created with a local `DateTime.now()`. Normalize both through
  // `toLocal()` so a non-UTC user doesn't see the displayed time shift when
  // the optimistic message is replaced by the parsed (UTC) record.
  String _time(DateTime t) {
    final local = t.toLocal();
    return '${local.hour.toString().padLeft(2, '0')}:'
        '${local.minute.toString().padLeft(2, '0')}';
  }
}

/// The bottom compose bar: a text field plus a send button.
class _Composer extends StatelessWidget {
  final TextEditingController controller;
  final VoidCallback onSend;
  const _Composer({required this.controller, required this.onSend});

  @override
  Widget build(BuildContext context) {
    return SafeArea(
      top: false,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(
          AppSpace.md,
          AppSpace.xs,
          AppSpace.md,
          AppSpace.sm,
        ),
        child: Row(
          children: [
            Expanded(
              child: TextField(
                controller: controller,
                textInputAction: TextInputAction.send,
                minLines: 1,
                maxLines: 5,
                decoration: const InputDecoration(
                  hintText: 'Message',
                  border: OutlineInputBorder(
                    borderRadius: BorderRadius.all(
                      Radius.circular(AppRadius.xl),
                    ),
                  ),
                  isDense: true,
                ),
                onSubmitted: (_) => onSend(),
              ),
            ),
            const Gap(AppSpace.xs),
            IconButton.filled(
              onPressed: onSend,
              icon: const Icon(Icons.send_rounded),
              tooltip: 'Send',
            ),
          ],
        ),
      ),
    );
  }
}
