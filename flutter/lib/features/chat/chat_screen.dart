import 'dart:async';

import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../platform/desktop_files.dart';
import '../../platform/open_path.dart';
import '../../platform/saf.dart';
import '../../sdk/models.dart';
import '../../state/app_scope.dart';
import '../../state/models.dart';
import '../../widgets/appear.dart';
import '../../widgets/common.dart';
import '../../widgets/processing.dart';

/// A one-to-one chat with [peer]. [peerId] is the discovered device's real
/// id — the conversation key. [PeerTarget] does carry its own `id` field
/// now, but it's optional (a manually-entered host:port target has none), so
/// the id is threaded through separately here rather than read off [peer].
///
/// The thread is reachable whether or not the peer is online: history is
/// local. Sending is not — a text message queues in the engine's outbox, and
/// a file (increment 2a) fails with a clear reason rather than promising a
/// delivery this build cannot make.
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

  /// Attach files to the conversation.
  ///
  /// [pickFilesToStage] is a MULTI-select: every file it returns gets its own
  /// row and its own transfer. Sending only the first while the user watched
  /// themselves choose five is silent data loss, so the loop is the point.
  Future<void> _attach() async {
    final chat = AppScope.of(context).chat;
    final picked = await withProcessing(
      context,
      'Preparing files…',
      pickFilesToStage,
    );
    if (picked.isEmpty || !mounted) return;
    for (final file in picked) {
      // Not awaited, and deliberately not sequential: each call appends its
      // optimistic row synchronously, so all of them appear at once.
      unawaited(
        chat.sendFile(
          widget.peerId,
          widget.peer,
          file.path,
          name: file.name,
          size: file.size,
        ),
      );
    }
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
                      itemBuilder: (context, i) {
                        final message = items[items.length - 1 - i];
                        // A row the engine refused to send exists only here,
                        // so only the user can clear it.
                        final unsent = state.chat.isUnsent(
                          widget.peerId,
                          message.id,
                        );
                        return Appear(
                          index: i,
                          child: _ChatBubble(
                            message: message,
                            error: state.chat.errorFor(message.id),
                            onDismiss: unsent
                                ? () => state.chat.dismiss(
                                    widget.peerId,
                                    message.id,
                                  )
                                : null,
                          ),
                        );
                      },
                    );
                  },
                ),
              ),
              _Composer(
                controller: _controller,
                onSend: _send,
                onAttach: _attach,
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// One message bubble: own messages lean right in `primaryContainer`, the
/// peer's lean left in `surfaceContainerHighest`. A file row renders its own
/// body ([_FileBody]) instead of the message text — a file record's `body` is
/// empty, so rendering it as text would be a blank bubble.
class _ChatBubble extends StatelessWidget {
  final ChatMessage message;

  /// Why this row failed, when the engine said so (never persisted).
  final String? error;

  /// Non-null only for a row the engine never persisted (a refused share):
  /// nothing else will ever clear it, so the user must be able to.
  final VoidCallback? onDismiss;
  const _ChatBubble({required this.message, this.error, this.onDismiss});

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
            child: Material(
              color: bg,
              borderRadius: BorderRadius.circular(AppRadius.lg),
              clipBehavior: Clip.antiAlias,
              child: InkWell(
                onTap: message.isFile && _openablePath(message) != null
                    ? () => _open(context, message)
                    : null,
                child: Padding(
                  padding: const EdgeInsets.symmetric(
                    horizontal: AppSpace.sm,
                    vertical: AppSpace.xs,
                  ),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      if (message.isFile)
                        _FileBody(message: message, fg: fg)
                      else
                        Text(
                          message.body,
                          style: text.bodyMedium?.copyWith(color: fg),
                        ),
                      if (error != null) ...[
                        const Gap(AppSpace.xxs),
                        Text(
                          error!,
                          style: text.labelSmall?.copyWith(color: scheme.error),
                        ),
                      ],
                      const Gap(AppSpace.xxs),
                      Row(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          Text(
                            _time(message.at),
                            style: text.labelSmall?.copyWith(
                              color: fg.withValues(alpha: 0.7),
                            ),
                          ),
                          if (mine) ...[
                            const Gap(AppSpace.xxs),
                            Icon(
                              _deliveryGlyph(message.status),
                              size: 14,
                              color: _failedStatus(message.status)
                                  ? scheme.error
                                  : fg.withValues(alpha: 0.7),
                            ),
                          ],
                        ],
                      ),
                      // Nothing else will ever clear this row — it exists only
                      // in this session, because the engine refused to send it
                      // and therefore persisted nothing. A full-size action,
                      // not a cramped glyph: it is the only way out.
                      if (onDismiss != null)
                        Align(
                          alignment: Alignment.centerRight,
                          child: TextButton(
                            onPressed: onDismiss,
                            child: const Text('Dismiss'),
                          ),
                        ),
                    ],
                  ),
                ),
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

/// Whether a status means the row did not, and will not, arrive.
bool _failedStatus(String status) => const {
  ChatStatusValue.failed,
  ChatStatusValue.declined,
  ChatStatusValue.interrupted,
}.contains(status);

/// The trailing marker on one's OWN row: what happened to it.
///
/// Deliberately not a two-state pending/tick test. Since files share a
/// conversation, an outgoing row can now be `transferring`, `failed`,
/// `declined` or `interrupted` — showing the delivered tick for any of those
/// would tell the user a file arrived when the same bubble says it failed.
/// The tick means delivered, and nothing else does.
IconData _deliveryGlyph(String status) => switch (status) {
  ChatStatusValue.pending ||
  ChatStatusValue.transferring ||
  ChatStatusValue.pendingApproval => Icons.schedule,
  ChatStatusValue.failed => Icons.error_outline_rounded,
  ChatStatusValue.declined => Icons.block_rounded,
  ChatStatusValue.interrupted => Icons.help_outline_rounded,
  // `sent` (and a text row's `received`) — the only delivered states.
  _ => Icons.check_rounded,
};

/// The path a settled file row can be opened at, or null when there is
/// nothing to open (still in flight, declined/failed, or a receive that
/// completed on an engine that recorded no path).
String? _openablePath(ChatMessage message) {
  const settled = {ChatStatusValue.sent, ChatStatusValue.received};
  if (!settled.contains(message.status)) return null;
  final path = message.localPath;
  return (path == null || path.isEmpty) ? null : path;
}

/// Open a received/sent file with the OS handler.
///
/// On Android the engine's own copy of a received file is deleted once it has
/// been published into the user's SAF folder, so the recorded path dangles.
/// That is not an error — the file is exactly where the user asked for it — so
/// fall back to opening it there by name, the same fallback the History screen
/// uses for the same reason.
Future<void> _open(BuildContext context, ChatMessage message) async {
  final error = await openLocalPath(_openablePath(message) ?? '');
  if (error == null) return;
  final name = message.fileName ?? '';
  if (!message.isMine &&
      name.isNotEmpty &&
      Saf.isSupported &&
      await Saf.open(name)) {
    return;
  }
  if (context.mounted) {
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(error)));
  }
}

/// A shared file's row: icon, name, size + status, and — while it is in
/// flight or awaiting a decision — progress or the approval actions.
///
/// Everything here is sourced from the PERSISTED [ChatMessage]. The live
/// [Transfer] is consulted only to overlay progress on an in-flight row, and
/// is expected to be absent (after a restart, or once the transfer settles).
class _FileBody extends StatelessWidget {
  final ChatMessage message;
  final Color fg;
  const _FileBody({required this.message, required this.fg});

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    final text = Theme.of(context).textTheme;
    final muted = fg.withValues(alpha: 0.7);
    final size = message.fileSize;
    final meta = [
      if (size != null && size > 0) formatBytes(size),
      _statusLabel(message),
    ].join(' · ');

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(_icon(message), size: AppIcons.md, color: fg),
            const Gap(AppSpace.xs),
            Flexible(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                mainAxisSize: MainAxisSize.min,
                children: [
                  Text(
                    message.fileName ?? 'File',
                    maxLines: 2,
                    overflow: TextOverflow.ellipsis,
                    style: text.bodyMedium?.copyWith(
                      color: fg,
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                  Text(
                    meta,
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: text.labelSmall?.copyWith(color: muted),
                  ),
                ],
              ),
            ),
          ],
        ),
        if (message.status == ChatStatusValue.transferring) ...[
          const Gap(AppSpace.xs),
          // The one place a live transfer is read: purely a progress overlay.
          // No live entry (a restart, or a row that settled) leaves an
          // indeterminate bar rather than a fabricated 0%.
          AnimatedBuilder(
            animation: state.transfer,
            builder: (context, _) {
              final live = state.transfer.byId(message.id);
              final total = (live != null && live.totalBytes > 0)
                  ? live.totalBytes
                  : (message.fileSize ?? 0);
              final value = (live == null || total <= 0)
                  ? null
                  : (live.doneBytes / total).clamp(0.0, 1.0).toDouble();
              return ClipRRect(
                borderRadius: BorderRadius.circular(AppRadius.sm),
                child: LinearProgressIndicator(
                  value: value,
                  minHeight: 4,
                  backgroundColor: fg.withValues(alpha: 0.15),
                ),
              );
            },
          ),
        ],
        // The peer is offering us a file. These are the ordinary transfer
        // approvals — the chat row's id IS the transfer's id, so they take it
        // unchanged, and there is no second approval path to keep in step.
        if (message.awaitingApproval) ...[
          const Gap(AppSpace.xxs),
          Wrap(
            spacing: AppSpace.xs,
            runSpacing: AppSpace.xxs,
            children: [
              TextButton(
                onPressed: () => state.transfer.reject(message.id),
                child: const Text('Decline'),
              ),
              FilledButton.tonal(
                onPressed: () => state.transfer.accept(message.id),
                child: const Text('Accept'),
              ),
              Tooltip(
                message: 'Accept and always trust this device',
                child: FilledButton(
                  onPressed: () => state.transfer.acceptTrust(message.id),
                  child: const Text('Trust'),
                ),
              ),
            ],
          ),
        ],
      ],
    );
  }

  /// The leading icon: what this row *is*, except when it went wrong — then it
  /// borrows the trailing marker's glyph so the two can never disagree about
  /// which failure state this is.
  IconData _icon(ChatMessage m) {
    if (_failedStatus(m.status)) return _deliveryGlyph(m.status);
    return m.status == ChatStatusValue.pendingApproval
        ? Icons.move_to_inbox_rounded
        : Icons.insert_drive_file_rounded;
  }

  /// Plain language for a record status, from the row's own point of view.
  String _statusLabel(ChatMessage m) => switch (m.status) {
    ChatStatusValue.transferring => m.isMine ? 'Sending…' : 'Receiving…',
    ChatStatusValue.sent => 'Sent',
    ChatStatusValue.received => 'Received · tap to open',
    ChatStatusValue.pendingApproval => m.isMine
        ? 'Waiting for approval'
        : 'Wants to send you this',
    ChatStatusValue.declined => 'Declined',
    ChatStatusValue.failed => 'Failed',
    ChatStatusValue.interrupted => 'Interrupted',
    ChatStatusValue.pending => 'Waiting',
    _ => m.status,
  };
}

/// The bottom compose bar: attach, a text field, and a send button.
class _Composer extends StatelessWidget {
  final TextEditingController controller;
  final VoidCallback onSend;
  final VoidCallback onAttach;
  const _Composer({
    required this.controller,
    required this.onSend,
    required this.onAttach,
  });

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
            IconButton(
              onPressed: onAttach,
              icon: const Icon(Icons.attach_file_rounded),
              tooltip: 'Attach files',
            ),
            const Gap(AppSpace.xxs),
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
