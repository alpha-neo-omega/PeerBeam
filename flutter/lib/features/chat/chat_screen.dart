import 'dart:async';

import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../platform/desktop_files.dart';
import '../../platform/open_path.dart';
import '../../platform/saf.dart';
import '../../sdk/error_text.dart';
import '../../sdk/models.dart';
import '../../state/app_scope.dart';
import '../../state/models.dart';
import '../../state/stores.dart';
import '../../widgets/appear.dart';
import '../../widgets/common.dart';
import '../../widgets/pairing.dart';
import '../../widgets/processing.dart';
import '../send/pick_device.dart';
import 'chat_drop_zone.dart';

/// A one-to-one chat with [peer]. [peerId] is the discovered device's real
/// id — the conversation key. [PeerTarget] does carry its own `id` field
/// now, but it's optional (a manually-entered host:port target has none), so
/// the id is threaded through separately here rather than read off [peer].
///
/// The thread is reachable whether or not the peer is online: history is
/// local, and since increment 2b both a text message and a file *share* queue
/// in the engine's outbox for an offline peer rather than failing.
///
/// One case is genuinely different, and is why the composer can be disabled at
/// all (see [_ChatScreenState._canSend]): a peer with no known address — a
/// conversation opened from the Conversations list for a device discovery
/// cannot currently see. The engine refuses such a send outright (`device_from`
/// requires an address and a port), before anything is enqueued, so the
/// composer says so instead of accepting messages that would silently never
/// exist.
///
/// That case is temporary, and the screen treats it as temporary: [peer] is
/// only the target this thread was *pushed* with, and the live one is re-read
/// from discovery on every build (see [_ChatScreenState._target]). A device
/// that comes back while its thread is open re-enables the composer where it
/// stands, which is what "Automatic reconnect" has to mean on the one surface
/// that exists to be open before the peer is.
class ChatScreen extends StatefulWidget {
  final String peerId;
  final PeerTarget peer;
  const ChatScreen({super.key, required this.peerId, required this.peer});

  @override
  State<ChatScreen> createState() => _ChatScreenState();
}

class _ChatScreenState extends State<ChatScreen> {
  final _controller = TextEditingController();

  /// Message ids the user has picked out of this thread.
  ///
  /// Selection **is** this set being non-empty — there is no separate mode
  /// flag, because every way in and out of selection here is a change to it:
  /// a long-press on the first bubble starts it, a tap toggles, and taking the
  /// last one back ends it. One piece of state cannot disagree with itself.
  ///
  /// It can outlive the rows it names — a message can be deleted from another
  /// surface, or a thread re-read while a selection sits open — so it is never
  /// rendered directly. `build` narrows it to ids still in the thread first and
  /// derives everything from that, rather than mutating state from inside a
  /// build; the ids the narrowing dropped are then taken out for good by
  /// [_pruneSelectionAfterFrame], once the frame is over. The same shape
  /// `TransfersScreen` uses for its own selection, and for the same reason:
  /// incoming messages and staging progress rebuild this screen constantly.
  Set<String> _selected = {};

  @override
  void initState() {
    super.initState();
    // Load once the first frame is up (not in initState directly — the
    // repository may not be attached to a live engine yet during boot, same
    // reasoning as the other repositories' "don't refresh in the constructor"
    // note).
    //
    // `openThread`, not `refresh`: opening is also when a row a crash left
    // mid-flight gets settled, and that has to happen before the thread is
    // rendered or a dead Accept button is offered for a transfer that no
    // longer exists.
    WidgetsBinding.instance.addPostFrameCallback((_) async {
      if (!mounted) return;
      final chat = AppScope.of(context).chat;
      await chat.openThread(widget.peerId);
      // Opening the thread is the moment it has been read. This sends nothing
      // unless the user opted into read receipts (default off), so calling it
      // unconditionally is not a disclosure — the engine owns that decision,
      // and duplicating it here would be a second place to get it wrong.
      await chat.markRead(widget.peerId);
    });
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  /// The peer as discovery can see it **right now**, falling back to the target
  /// this thread was opened with.
  ///
  /// [ChatScreen.peer] is frozen at push time, and the Conversations list
  /// deliberately pushes an address-less placeholder for a peer discovery
  /// cannot currently see — which is exactly the thread this screen exists to
  /// keep reachable. Read once, that placeholder never expires: the device
  /// appears on Home, the engine can reach it again, and the composer, the
  /// attach button and the drop zone all stay dead until the user backs out and
  /// comes in again, with nothing on screen suggesting they should.
  ///
  /// Discovery wins whenever it has an answer, rather than only filling a gap:
  /// it carries the peer's *current* address and name, and the pushed target
  /// may be minutes old.
  PeerTarget _target(AppState state) =>
      state.device.peerTarget(widget.peerId) ?? widget.peer;

  /// Whether [peer] can be sent to at all.
  ///
  /// Mirrors the engine's own precondition (`device_from`: at least one address
  /// and a nonzero port) rather than guessing at reachability — an *offline*
  /// peer with a known address is perfectly sendable, because the outbox holds
  /// the message until it returns. What cannot work is a peer we have no
  /// address for: that send is refused before it is enqueued, and the
  /// repository would be left holding an optimistic bubble that the next
  /// refresh silently deletes.
  ///
  /// Takes the peer rather than reading [ChatScreen.peer]: this question has to
  /// be re-asked of the *live* target every build, or its answer outlives the
  /// only condition that made it true.
  static bool _canSend(PeerTarget peer) =>
      peer.addresses.isNotEmpty && peer.port > 0;

  /// Fire-and-forget: `send` awaits a synchronous dial+handshake under the
  /// hood, so the button handler must not block on it — the optimistic
  /// message (appended inside the repository, before its own await) is what
  /// keeps the tap responsive.
  void _send() {
    final text = _controller.text;
    if (text.trim().isEmpty) return;
    final state = AppScope.of(context);
    _controller.clear();
    // Resolved as the message goes out, not as the thread was opened: an
    // address that arrived in between is the address this send needs.
    state.chat.send(widget.peerId, _target(state), text);
  }

  /// Attach files to the conversation.
  ///
  /// Asks *what kind* first (Document / Photos & videos / Audio — see
  /// [_pickAttachKind]), then hands that straight to [pickFilesToStage] so
  /// the platform layer applies the matching OS filter. Everything past that
  /// is unchanged: [pickFilesToStage] is still a MULTI-select, and every file
  /// it returns still gets its own row and its own transfer. Sending only the
  /// first while the user watched themselves choose five is silent data loss,
  /// so the loop is the point.
  Future<void> _attach() async {
    final scope = AppScope.of(context);
    final chat = scope.chat;
    final kind = await _pickAttachKind(context);
    if (kind == null || !mounted) return;
    // `keep` is what the whole app still has staged, not what this screen
    // does. Android prunes the picked-files cache on any pick, from any flow,
    // so a chat attachment that did not name the Send flow's staged batch
    // could age it out from under a decision the user has not made yet. The
    // chat's own attachments need no keeping — they are handed to the engine
    // immediately, and the per-batch directory already stops a later pick
    // reaching into one still being staged.
    final picked = await withProcessing(
      context,
      'Preparing files…',
      () => pickFilesToStage(kind: kind, keep: scope.staging.paths),
    );
    if (picked.isEmpty || !mounted) return;
    // After the picker, not before it: choosing files can take a while, and the
    // peer may have been discovered (or moved) in the meantime.
    final peer = _target(scope);
    for (final file in picked) {
      // Not awaited, and deliberately not sequential: each call appends its
      // optimistic row synchronously, so all of them appear at once.
      unawaited(
        chat.sendFile(
          widget.peerId,
          peer,
          file.path,
          name: file.name,
          size: file.size,
        ),
      );
    }
  }

  /// Add or remove one message — which is also how selection begins and ends.
  ///
  /// The reactions offered by the picker. Six, because the point is one tap on
  /// something already visible; anything longer is a search, and a message
  /// worth a rarer emoji is worth typing a reply.
  static const _quickReactions = [
    '\u{1F44D}',
    '\u{2764}',
    '\u{1F602}',
    '\u{1F389}',
    '\u{1F440}',
    '\u{1F622}',
  ];

  /// Apply a reaction and say so only when it did **not** reach the peer.
  ///
  /// Silence on success is deliberate: a reaction is a small gesture and a
  /// toast for every one would be louder than the thing it reports. But a
  /// reaction the peer never saw — offline, or a build too old to have
  /// negotiated them — would otherwise look identical to one that landed, and
  /// that is the case the user cannot recover from without being told.
  Future<void> _react(
    String messageId,
    String emoji, {
    required bool remove,
  }) async {
    final state = AppScope.of(context);
    final delivered = await state.chat.react(
      widget.peerId,
      messageId,
      emoji,
      remove: remove,
    );
    if (!mounted || delivered) return;
    ScaffoldMessenger.of(context).showSnackBar(
      const SnackBar(
        content: Text(
          'Saved here, but not delivered — the device is offline or its '
          'app is too old for reactions.',
        ),
      ),
    );
  }

  /// Offer the quick reactions for [messageId]. Opened by a double-tap: a tap
  /// already opens a file and a long-press already starts a selection, so this
  /// is the gesture left that costs neither.
  Future<void> _pickReaction(ChatMessage message) async {
    final chosen = await showModalBottomSheet<String>(
      context: context,
      builder: (sheetContext) => SafeArea(
        child: Padding(
          padding: const EdgeInsets.all(AppSpace.md),
          child: Row(
            mainAxisAlignment: MainAxisAlignment.spaceEvenly,
            children: [
              for (final e in _quickReactions)
                IconButton(
                  onPressed: () => Navigator.of(sheetContext).pop(e),
                  icon: Text(e, style: const TextStyle(fontSize: 24)),
                  tooltip: 'React with $e',
                ),
            ],
          ),
        ),
      ),
    );
    if (chosen == null) return;
    // Tapping an emoji this side already used withdraws it, matching what the
    // chip under the bubble does — one meaning per gesture.
    final mine = message.reactions.any((r) => r.emoji == chosen && r.isMine);
    await _react(message.id, chosen, remove: mine);
  }

  /// A long-press (or a secondary tap, the desktop idiom) on a bubble with
  /// nothing selected lands here and leaves exactly that message selected; a
  /// plain tap while selecting toggles; taking the last one back empties the
  /// set and the selection bar goes with it.
  void _toggle(String id) => setState(() {
    final next = Set<String>.from(_selected);
    if (!next.remove(id)) next.add(id);
    _selected = next;
  });

  void _clearSelection() => setState(() => _selected = {});

  /// Take out every id the thread no longer holds — after the frame, never
  /// during one.
  ///
  /// `build` only ever *narrows* the set for rendering, which is not the same
  /// as pruning it: select two messages, have one of them deleted from
  /// elsewhere, and `_selected` goes on naming an id nothing in the thread
  /// matches, because the other one survived. An inbound file's message id is
  /// the **sender's** `FileRef` id — a value the peer chooses, not one this
  /// device mints — so a peer that reuses that id gets a message the user never
  /// picked rendered as already selected, counted in "2 selected", and carried
  /// off to a third device by the next Forward.
  ///
  /// So every stale id goes, not only the case where the whole set went stale
  /// at once. Emptying the set is how selection ends, so a thread that lost
  /// every selected row still leaves selection — by the same path rather than
  /// by a second one.
  ///
  /// **Both** halves are re-read when the callback runs rather than trusted
  /// from when it was scheduled: a rebuild can land in the gap, and pruning
  /// against the thread as it was then would take out ids that have since come
  /// back.
  void _pruneSelectionAfterFrame() {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted || _selected.isEmpty) return;
      final present = AppScope.of(
        context,
      ).chat.messagesFor(widget.peerId).map((m) => m.id).toSet();
      final narrowed = _selected.intersection(present);
      if (narrowed.length == _selected.length) return;
      setState(() => _selected = narrowed);
    });
  }

  void _snack(ScaffoldMessengerState messenger, String message) => messenger
    ..hideCurrentSnackBar()
    ..showSnackBar(SnackBar(content: Text(message)));

  /// Confirm, then delete the selected messages from this device.
  ///
  /// The confirmation promises exactly two things, because two are all this can
  /// honestly promise beforehand: they go **from this device**, and anything
  /// still waiting to be sent is kept and still sent. No counts of what will
  /// survive — what is queued can change up to the moment the engine deletes —
  /// so those are reported afterwards, from the engine's own answer.
  Future<void> _deleteSelected(List<String> ids) async {
    final messenger = ScaffoldMessenger.of(context);
    final chat = AppScope.of(context).chat;
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: Text(
          ids.length == 1
              ? 'Delete 1 message?'
              : 'Delete ${ids.length} messages?',
        ),
        content: const Text(
          'This removes them from this device. Anything still waiting to be '
          'sent is kept and will still be sent.\n\n'
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
    try {
      final result = await chat.deleteMessages(widget.peerId, ids);
      if (!mounted) return;
      _clearSelection();
      _snack(messenger, _deleteOutcome(result));
    } catch (e) {
      // Reported, never swallowed: a refusal here is the engine declining to
      // delete because it could not establish what is still queued, and the
      // messages are all still there. Saying nothing would look like it worked.
      _snack(messenger, 'Could not delete — ${friendlyError(e)}');
    }
  }

  /// What actually happened, in the engine's own answer.
  ///
  /// Never a restatement of what was asked. A kept row is one still waiting to
  /// be sent — the engine refused to take it because its record is what will
  /// deliver the file — so the user is told that in their own terms rather than
  /// left to wonder why a bubble they deleted is still on screen.
  static String _deleteOutcome(({int removed, List<String> kept}) r) {
    final removed = r.removed == 1
        ? 'Deleted 1 message'
        : 'Deleted ${r.removed} messages';
    if (r.kept.isEmpty) return removed;
    final kept = r.kept.length == 1
        ? '1 kept because it is still being sent'
        : '${r.kept.length} kept because they are still being sent';
    return '$removed · $kept';
  }

  /// Forward the selected messages to another device.
  ///
  /// [chosen] arrives in **thread order** and is sent in that order, one await
  /// at a time: forwarding a conversation whose messages arrive shuffled is a
  /// different conversation. Nothing new crosses the engine boundary — a text
  /// message is sent as text and a file as a file, through the same two calls
  /// the composer and the attach button already use.
  Future<void> _forwardSelected(List<ChatMessage> chosen) async {
    final messenger = ScaffoldMessenger.of(context);
    final scope = AppScope.of(context);
    final picked = await showDevicePicker(context);
    if (picked == null || !mounted) return;

    // A conversation may only be keyed by a peer's **real** device id. A saved
    // (by-address) entry's id is a locally minted timestamp the peer has never
    // heard of, so filing rows under it would leave a thread every inbound
    // record misses — the rule the Home screen already applies before it opens
    // a chat at all. So the pick is resolved back to a discovered identity, and
    // refused honestly when there is none.
    final peerId = _realPeerId(scope, picked.target);
    final target = peerId == null ? null : scope.device.peerTarget(peerId);
    if (peerId == null || target == null) {
      _snack(
        messenger,
        'Cannot forward to ${picked.name} yet — PeerBeam has to see the device '
        'before it knows which conversation to file these under.',
      );
      return;
    }

    // Split before sending anything, never per message. A file whose bytes are
    // no longer on this device cannot be forwarded at all, and handing the
    // engine a path that is not there would produce a row of failed bubbles in
    // the other thread having warned nobody.
    final sendable = <ChatMessage>[];
    final missing = <String>[];
    for (final m in chosen) {
      final path = m.isFile ? (m.localPath ?? '') : '';
      if (!m.isFile || localFileExists(path)) {
        sendable.add(m);
      } else {
        missing.add(m.fileName ?? 'That file');
      }
    }
    if (sendable.isEmpty) {
      _snack(
        messenger,
        'Nothing could be forwarded — ${_missingText(missing)}',
      );
      return;
    }

    // Selection ends the moment the batch is committed, so a second tap cannot
    // send it twice while the first is still going out.
    _clearSelection();
    final chat = scope.chat;
    for (final m in sendable) {
      if (m.isFile) {
        await chat.sendFile(
          peerId,
          target,
          m.localPath ?? '',
          name: m.fileName,
          size: m.fileSize,
        );
      } else {
        await chat.send(peerId, target, m.body);
      }
    }
    final sent = sendable.length == 1
        ? 'Forwarded 1 message to ${picked.name}'
        : 'Forwarded ${sendable.length} messages to ${picked.name}';
    _snack(
      messenger,
      missing.isEmpty ? sent : '$sent · ${_missingText(missing)}',
    );
  }

  /// The peer's real device id behind a picked target, or null when discovery
  /// cannot currently see it. A discovered pick answers itself; a saved one is
  /// resolved by the address it advertises, exactly as the Home screen does.
  String? _realPeerId(AppState scope, PeerTarget target) {
    final id = target.id;
    if (id != null && scope.device.peerTarget(id) != null) return id;
    if (target.addresses.isEmpty) return null;
    return scope.device
        .deviceAtAddress(target.addresses.first, target.port)
        ?.id;
  }

  /// Names the files that could not go, and why — never a bare count, because
  /// "1 was skipped" tells the user nothing they can act on.
  static String _missingText(List<String> names) => names.length == 1
      ? "${names.single} isn't on this device any more"
      : "${names.join(', ')} aren't on this device any more";

  /// The app bar while messages are selected: leave, the count, and the two
  /// things that can be done to a selection.
  ///
  /// [items] arrives in thread order, so [_forwardSelected] gets its messages
  /// in the order they appear rather than in whatever order they were tapped.
  PreferredSizeWidget _selectionBar(
    List<ChatMessage> items,
    Set<String> selected,
  ) {
    final chosen = items
        .where((m) => selected.contains(m.id))
        .toList(growable: false);
    return AppBar(
      leading: IconButton(
        icon: const Icon(Icons.close_rounded),
        tooltip: 'Cancel selection',
        onPressed: _clearSelection,
      ),
      title: Text('${selected.length} selected'),
      actions: [
        IconButton(
          icon: const Icon(Icons.forward_rounded),
          tooltip: 'Forward',
          onPressed: () => _forwardSelected(chosen),
        ),
        IconButton(
          icon: const Icon(Icons.delete_outline_rounded),
          tooltip: 'Delete',
          onPressed: () =>
              _deleteSelected(chosen.map((m) => m.id).toList(growable: false)),
        ),
      ],
    );
  }

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    // The whole screen rebuilds on a chat change, not just the list: the app
    // bar becomes the selection bar, and both it and the bubbles have to be
    // reading the same narrowed selection within one frame.
    //
    // Discovery is merged in for a second reason: the send target is re-read
    // from it every build (see [_target]), and nothing else would ever notice
    // it move. `AppScope` is a plain `InheritedWidget` around a state object
    // whose identity never changes, so a peer coming back would otherwise leave
    // this screen — the one disabled on its account — sitting on a snapshot
    // taken before it existed. The same merge the device picker was fixed with.
    return AnimatedBuilder(
      animation: Listenable.merge([state.chat, state.device]),
      builder: (context, _) {
        final peer = _target(state);
        final items = state.chat.messagesFor(widget.peerId);
        // Derived every build, never written back into state (see [_selected]).
        final present = items.map((m) => m.id).toSet();
        final selected = _selected.intersection(present);
        if (selected.length != _selected.length) _pruneSelectionAfterFrame();
        final selecting = selected.isNotEmpty;
        return PopScope(
          // Back leaves the selection before it leaves the conversation. A back
          // press that closed the whole screen with a selection open would
          // throw away work the user is in the middle of and land them
          // somewhere they did not ask to be.
          canPop: !selecting,
          onPopInvokedWithResult: (didPop, _) {
            if (!didPop) _clearSelection();
          },
          child: _body(state, peer, items, selected, selecting),
        );
      },
    );
  }

  Widget _body(
    AppState state,
    PeerTarget peer,
    List<ChatMessage> items,
    Set<String> selected,
    bool selecting,
  ) {
    final canSend = _canSend(peer);
    // Why the thread is empty, when it is empty because a read failed.
    final failure = state.chat.loadErrorFor(widget.peerId);
    return Scaffold(
      appBar: selecting
          ? _selectionBar(items, selected)
          : AppBar(title: Text(peer.name)),
      // Desktop-only drag & drop for this one conversation — a transparent
      // passthrough everywhere else, exactly like the Send flow's own
      // DropZone. Wraps the body (not the AppBar) so a drop anywhere over the
      // thread or composer sends straight to this peer.
      body: ChatDropZone(
        peerId: widget.peerId,
        peer: peer,
        canSend: canSend,
        child: SafeArea(
          child: ContentPane(
            child: Column(
              children: [
                Expanded(
                  child: items.isEmpty
                      // A conversation this device could not read is not a
                      // conversation with nothing in it, and "No messages yet"
                      // is a statement about the user's own history that a
                      // failed read has no grounds to make. Only when there is
                      // genuinely nothing on screen: a thread that loaded once
                      // and failed to reload keeps its messages, because stale
                      // messages beat an error page over messages that are
                      // right there.
                      ? (failure != null
                            ? ErrorState(
                                error: failure,
                                title: 'Could not open this conversation',
                                onRetry: () =>
                                    state.chat.openThread(widget.peerId),
                              )
                            : const EmptyState(
                                icon: Icons.chat_bubble_outline_rounded,
                                title: 'No messages yet',
                                message:
                                    'Send a message to start the conversation.',
                              ))
                      // Reversed so the latest message stays pinned to the
                      // bottom without a manual scroll controller.
                      : ListView.builder(
                          reverse: true,
                          padding: const EdgeInsets.all(AppSpace.md),
                          itemCount: items.length,
                          itemBuilder: (context, i) {
                            final message = items[items.length - 1 - i];
                            // A row the engine refused to send exists only
                            // here, so only the user can clear it.
                            final unsent = state.chat.isUnsent(
                              widget.peerId,
                              message.id,
                            );
                            return Appear(
                              index: i,
                              child: _ChatBubble(
                                message: message,
                                error: state.chat.errorFor(message.id),
                                selecting: selecting,
                                selected: selected.contains(message.id),
                                onToggle: () => _toggle(message.id),
                                onDismiss: unsent
                                    ? () => state.chat.dismiss(
                                        widget.peerId,
                                        message.id,
                                      )
                                    : null,
                                onReact: (emoji, {required remove}) =>
                                    _react(message.id, emoji, remove: remove),
                                onPickReaction: () => _pickReaction(message),
                              ),
                            );
                          },
                        ),
                ),
                if (!canSend)
                  Padding(
                    padding: const EdgeInsets.fromLTRB(
                      AppSpace.md,
                      0,
                      AppSpace.md,
                      AppSpace.xxs,
                    ),
                    child: Text(
                      'No address known for ${peer.name} yet — this '
                      'conversation is readable, and sending works again as soon '
                      'as the device is discovered.',
                      style: Theme.of(context).textTheme.labelSmall?.copyWith(
                        color: Theme.of(context).colorScheme.onSurfaceVariant,
                      ),
                    ),
                  ),
                _Composer(
                  controller: _controller,
                  onSend: _send,
                  onAttach: _attach,
                  enabled: canSend,
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// The reactions on one message, grouped by emoji with a count.
///
/// Grouped rather than listed one-per-reaction because a conversation has two
/// participants: the interesting facts are *which* emoji and *whether I am one
/// of the people who used it*, and a row of identical glyphs says neither
/// clearly. A chip this side has reacted with is outlined, so tapping to
/// withdraw is aimed at something visible rather than remembered.
class _Reactions extends StatelessWidget {
  final List<ChatReaction> reactions;
  final Color fg;
  final void Function(String emoji, {required bool remove})? onTap;

  const _Reactions({required this.reactions, required this.fg, this.onTap});

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    // Insertion-ordered, so the chips do not reshuffle when one is added.
    final groups = <String, ({int count, bool mine})>{};
    for (final r in reactions) {
      final g = groups[r.emoji];
      groups[r.emoji] = (
        count: (g?.count ?? 0) + 1,
        mine: (g?.mine ?? false) || r.isMine,
      );
    }
    return Wrap(
      spacing: AppSpace.xxs,
      runSpacing: AppSpace.xxs,
      children: [
        for (final e in groups.entries)
          InkWell(
            onTap: onTap == null
                ? null
                : () => onTap!(e.key, remove: e.value.mine),
            borderRadius: BorderRadius.circular(AppRadius.sm),
            child: Container(
              padding: const EdgeInsets.symmetric(
                horizontal: AppSpace.xs,
                vertical: 1,
              ),
              decoration: BoxDecoration(
                color: fg.withValues(alpha: 0.08),
                borderRadius: BorderRadius.circular(AppRadius.sm),
                border: e.value.mine
                    ? Border.all(color: scheme.primary, width: 1)
                    : null,
              ),
              child: Text(
                e.value.count > 1 ? '${e.key} ${e.value.count}' : e.key,
                style: text.labelSmall?.copyWith(color: fg),
              ),
            ),
          ),
      ],
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

  /// Whether the screen is in selection mode. While it is, a plain tap toggles
  /// this bubble and the in-bubble actions (Cancel, Dismiss, the approval
  /// buttons) are withheld: the tap is the one action path on screen, and a
  /// live Accept under a selection bar is a second, unrelated decision sitting
  /// exactly where the user is aiming. The same rule `TransfersScreen` applies
  /// to its cards while selecting.
  final bool selecting;

  /// Whether this message is in the current (thread-narrowed) selection.
  final bool selected;

  /// Add or remove this message — a long-press or secondary tap starts the
  /// selection with it, and a tap toggles it once one is open.
  final VoidCallback onToggle;

  /// Non-null only for a row the engine never persisted (a refused share):
  /// nothing else will ever clear it, so the user must be able to.
  final VoidCallback? onDismiss;

  /// React to this message with the given emoji, or withdraw it if this side
  /// has already reacted with it. Null while selecting — a tap is the one
  /// action path on screen then.
  final void Function(String emoji, {required bool remove})? onReact;

  /// Open the quick-reaction picker for this message. Null while selecting.
  final VoidCallback? onPickReaction;
  const _ChatBubble({
    required this.message,
    required this.selecting,
    required this.selected,
    required this.onToggle,
    this.error,
    this.onDismiss,
    this.onReact,
    this.onPickReaction,
  });

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    final mine = message.isMine;
    final bg = mine ? scheme.primaryContainer : scheme.surfaceContainerHighest;
    final fg = mine ? scheme.onPrimaryContainer : scheme.onSurface;

    return Container(
      // The tint spans the whole row rather than the bubble, so a one-word
      // message is as legibly selected as a long one.
      color: selected ? scheme.primary.withValues(alpha: 0.12) : null,
      padding: const EdgeInsets.only(bottom: AppSpace.xs),
      child: Row(
        mainAxisAlignment: mine
            ? MainAxisAlignment.end
            : MainAxisAlignment.start,
        children: [
          if (selecting) ...[
            Icon(
              selected
                  ? Icons.check_circle_rounded
                  : Icons.radio_button_unchecked_rounded,
              size: AppIcons.md,
              color: selected ? scheme.primary : scheme.onSurfaceVariant,
            ),
            const Gap(AppSpace.xs),
          ],
          ConstrainedBox(
            constraints: BoxConstraints(
              maxWidth: MediaQuery.sizeOf(context).width * 0.75,
            ),
            child: Material(
              color: bg,
              borderRadius: BorderRadius.circular(AppRadius.lg),
              clipBehavior: Clip.antiAlias,
              child: InkWell(
                onTap: selecting
                    ? onToggle
                    : (message.isFile && _openablePath(message) != null
                          ? () => _open(context, message)
                          : null),
                // Long-press is the touch idiom for entering selection; a
                // long-press with a mouse is not, so desktop gets the
                // right-click it expects. Both land in the same toggle, so the
                // first one selects and any later one just adds or removes.
                onLongPress: onToggle,
                onSecondaryTap: onToggle,
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
                        _FileBody(
                          message: message,
                          fg: fg,
                          selecting: selecting,
                        )
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
                              // A read message earns its own glyph rather than
                              // a second tick: "delivered" and "read" are
                              // different claims, and only one of them is
                              // something the peer chose to tell us.
                              message.readAt != null
                                  ? Icons.done_all_rounded
                                  : _deliveryGlyph(message.status),
                              size: 14,
                              color: message.readAt != null
                                  ? scheme.primary
                                  : (_failedStatus(message.status)
                                        ? scheme.error
                                        : fg.withValues(alpha: 0.7)),
                            ),
                          ],
                          // An explicit control rather than a gesture. A
                          // double-tap here would put a double-tap recognizer
                          // in the arena around the whole bubble, which delays
                          // every tap inside it — including this row's Accept
                          // and Decline — by the double-tap timeout. A visible
                          // button costs a few pixels and no latency.
                          if (onPickReaction != null && !selecting) ...[
                            const Gap(AppSpace.xxs),
                            InkWell(
                              onTap: onPickReaction,
                              borderRadius: BorderRadius.circular(AppRadius.sm),
                              child: Padding(
                                padding: const EdgeInsets.all(2),
                                child: Icon(
                                  Icons.add_reaction_outlined,
                                  size: 14,
                                  color: fg.withValues(alpha: 0.7),
                                ),
                              ),
                            ),
                          ],
                        ],
                      ),
                      if (message.reactions.isNotEmpty) ...[
                        const Gap(AppSpace.xxs),
                        _Reactions(
                          reactions: message.reactions,
                          fg: fg,
                          onTap: selecting ? null : onReact,
                        ),
                      ],
                      // Nothing else will ever clear this row — it exists only
                      // in this session, because the engine refused to send it
                      // and therefore persisted nothing. A full-size action,
                      // not a cramped glyph: it is the only way out. Withheld
                      // while selecting, like every other in-bubble action.
                      if (onDismiss != null && !selecting)
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
/// conversation, an outgoing row can now be `staging`, `transferring`,
/// `failed`, `declined` or `interrupted` — showing the delivered tick for any
/// of those would tell the user a file arrived when the same bubble says it is
/// still being copied, or failed. The tick means delivered, and nothing else
/// does; every new status must be added here rather than left to fall into the
/// default.
IconData _deliveryGlyph(String status) => switch (status) {
  ChatStatusValue.staging ||
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

  /// Whether the screen is in selection mode — see [_ChatBubble.selecting].
  /// Cancel and the approval buttons are withheld while it is: they are
  /// decisions about this one file, and a selection bar is about several
  /// messages, so offering both at once invites the wrong tap.
  final bool selecting;
  const _FileBody({
    required this.message,
    required this.fg,
    required this.selecting,
  });

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
        // The engine is copying the file into the outbox's own storage. That
        // can run for minutes on a multi-GB pick, so the bar is determinate
        // wherever possible — an attach that looks hung is a bug report, and a
        // spinner that never moves is indistinguishable from one. The fraction
        // comes off the `chat_status` events' `progress` object (~100 of them
        // over a whole copy); before the first one lands there is genuinely
        // nothing to say, and an indeterminate bar says exactly that rather
        // than inventing 0%.
        //
        // No `AnimatedBuilder` here: the whole list is already rebuilt by the
        // screen's own listener on this same repository.
        if (message.status == ChatStatusValue.staging) ...[
          const Gap(AppSpace.xs),
          Builder(
            builder: (context) {
              final staged = state.chat.stagingFor(message.id);
              // Clamped, not trusted: the source can be appended to while it
              // is copied, so `done` may legitimately overshoot `total`.
              final value = (staged == null || staged.total <= 0)
                  ? null
                  : (staged.done / staged.total).clamp(0.0, 1.0).toDouble();
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
        // Staged or queued, the user can still call this off — and must be able
        // to: waiting out an 8 GiB copy of the wrong file is not a choice.
        // Wired to the engine's own cancel, whose answer is believed rather
        // than assumed (see [_cancel]).
        if (_cancellable(message) && !selecting)
          Align(
            alignment: Alignment.centerRight,
            child: TextButton(
              onPressed: () => _cancel(context, message),
              child: const Text('Cancel'),
            ),
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
        //
        // Gated on the LIVE transfer as well as the persisted status, the same
        // way the Transfers screen gates its own approval actions. The
        // persisted status alone is not enough: it is written by the engine and
        // reaches us on a `chat_status` event that necessarily lands *after*
        // the `transfer_started` announcing the bytes are already moving, and
        // under auto-accept the engine never opened a decision at all (no
        // `pending` entry), so Decline would be a no-op from the first frame —
        // a rendered consent control for a decision that was never asked and
        // cannot be revoked here. See [_offersApproval].
        if (message.awaitingApproval && !selecting)
          AnimatedBuilder(
            animation: state.transfer,
            builder: (context, _) {
              if (!_offersApproval(state.transfer.byId(message.id))) {
                return const SizedBox.shrink();
              }
              // The live transfer behind this row, if the engine has one. It
              // carries the handshake facts the chat offer itself never had:
              // whether this is first contact, and the pairing code to check
              // it with. Null is ordinary here — `_offersApproval` treats it as
              // permissive on purpose, since a `FileRef` can arrive on the chat
              // channel before its transfer's first frame does — and a row with
              // no entry has no first-contact record either, so it is not
              // gated. See `acceptWithPairingCheck` for what a null means once
              // a confirmation IS required.
              final live = state.transfer.byId(message.id);
              return Padding(
                padding: const EdgeInsets.only(top: AppSpace.xxs),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    // A file offered in a conversation is approved right here,
                    // so first contact has to be visible here too. Routing this
                    // decision around the check — on the grounds that a chat
                    // implies familiarity — would leave the gate guarding one
                    // of the two ways a file gets accepted.
                    if (live != null && live.newlyTrusted) ...[
                      PairingCodePanel(transfer: live),
                      const Gap(AppSpace.xxs),
                    ],
                    Wrap(
                      spacing: AppSpace.xs,
                      runSpacing: AppSpace.xxs,
                      children: [
                        TextButton(
                          onPressed: () => state.transfer.reject(message.id),
                          child: const Text('Decline'),
                        ),
                        FilledButton.tonal(
                          onPressed: () => acceptWithPairingCheck(
                            context,
                            live,
                            needsConfirmation: state.transfer
                                .needsPairingConfirmation(message.id),
                            accept: ({required confirmed}) => state.transfer
                                .accept(message.id, confirmed: confirmed),
                          ),
                          child: const Text('Accept'),
                        ),
                        Tooltip(
                          message: 'Accept and always trust this device',
                          child: FilledButton(
                            onPressed: () => acceptWithPairingCheck(
                              context,
                              live,
                              needsConfirmation: state.transfer
                                  .needsPairingConfirmation(message.id),
                              accept: ({required confirmed}) =>
                                  state.transfer.acceptTrust(
                                    message.id,
                                    confirmed: confirmed,
                                  ),
                            ),
                            child: const Text('Trust'),
                          ),
                        ),
                      ],
                    ),
                  ],
                ),
              );
            },
          ),
      ],
    );
  }

  /// Whether a decision is still genuinely open, given this row's live
  /// transfer ([Transfer]) — `null` when the engine has no entry for it.
  ///
  /// A null is deliberately permissive: the live map is ephemeral and empty
  /// after a restart, and there is a real window where the peer's `FileRef`
  /// has arrived on the CHAT channel but its first TRANSFER frame has not, so
  /// no entry exists yet. Refusing to render buttons there would hide a
  /// legitimate offer. (A row genuinely orphaned by a crash is settled by the
  /// engine's reconcile when the thread opens, so it never reaches here still
  /// reading `pendingapproval`.)
  ///
  /// Anything past [TransferState.pending] means the answer is in and the
  /// bytes are moving: no buttons.
  ///
  /// Two shorter windows are permissive for the same reason and are left so
  /// deliberately: between `transfer_queued` and `transfer_started` under
  /// auto-accept (bounded by one synchronous trust lookup), and between
  /// `transfer_completed` — which drops the live entry, so this reads `null`
  /// again — and the `chat_status: received` that settles the row. Both are
  /// sub-frame in practice and neither can outlive the event pair that closes
  /// it.
  bool _offersApproval(Transfer? live) =>
      live == null || live.state == TransferState.pending;

  /// Whether the local user may still call this share off here.
  ///
  /// A narrowing of the engine's own rule
  /// (`ChatRecord::is_cancellable_outgoing_file`) to the two states this
  /// surface offers the action in: **our own outgoing file**, staged or queued.
  /// A row whose bytes are already moving is cancellable by the engine too, but
  /// it has a live transfer and is called off from the Transfers screen (the
  /// chat message id IS the transfer id), so a second control for it here would
  /// be a second path to the same stop. An inbound offer is never cancelled —
  /// it is *declined*, at the approval gate.
  bool _cancellable(ChatMessage m) =>
      m.isMine &&
      m.isFile &&
      (m.status == ChatStatusValue.staging ||
          m.status == ChatStatusValue.pending);

  /// Ask the engine to call the share off, and say so if it could not.
  ///
  /// The row is deliberately NOT removed or relabelled optimistically: the
  /// engine answers `{cancelled: false}` when the file had already been
  /// delivered or declined, and pretending otherwise would tell the user they
  /// stopped something they did not. On a false the repository re-reads the
  /// conversation, so the row snaps to what it really is, and this says why the
  /// button appeared to do nothing.
  Future<void> _cancel(BuildContext context, ChatMessage m) async {
    // Captured before the await: the bubble can be rebuilt (or gone) by the
    // time the engine answers.
    final messenger = ScaffoldMessenger.of(context);
    final chat = AppScope.of(context).chat;
    final cancelled = await chat.cancelFile(m.peerId, m.id);
    if (cancelled) return;
    final name = m.fileName ?? 'that file';
    messenger
      ..hideCurrentSnackBar()
      ..showSnackBar(
        SnackBar(
          content: Text(
            'Could not cancel $name — it is no longer waiting to be sent.',
          ),
        ),
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
  ///
  /// Only ever called for a FILE row (this is [_FileBody]'s own label), which
  /// is why `pending` reads "Queued" here: on a file it means the bytes are
  /// staged and the entry is waiting for the peer, and calling that "Sent"
  /// — or even "Waiting", text's own word for the same status — would understate
  /// a file that is sitting on this disk. Text rows carry no status text at all;
  /// their marker is the trailing glyph.
  String _statusLabel(ChatMessage m) => switch (m.status) {
    ChatStatusValue.staging => 'Staging…',
    ChatStatusValue.transferring => m.isMine ? 'Sending…' : 'Receiving…',
    ChatStatusValue.sent => 'Sent',
    ChatStatusValue.received => 'Received · tap to open',
    ChatStatusValue.pendingApproval =>
      m.isMine ? 'Waiting for approval' : 'Wants to send you this',
    ChatStatusValue.declined => 'Declined',
    ChatStatusValue.failed => 'Failed',
    ChatStatusValue.interrupted => 'Interrupted',
    ChatStatusValue.pending => 'Queued',
    _ => m.status,
  };
}

/// Ask what kind of file to attach, in the shape WhatsApp uses: **Document**
/// (no filter — today's picker, unchanged), **Photos & videos**, or **Audio**.
/// Returns null if the sheet is dismissed without a choice.
///
/// Deliberately three choices and no more: there is no camera capture here,
/// because there is no camera dependency in this project — adding one is a
/// separate feature, not a fourth row bolted on here.
Future<AttachKind?> _pickAttachKind(BuildContext context) {
  return showModalBottomSheet<AttachKind>(
    context: context,
    showDragHandle: true,
    builder: (ctx) {
      final text = Theme.of(ctx).textTheme;
      return SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(
                AppSpace.lg,
                AppSpace.xxs,
                AppSpace.lg,
                AppSpace.xs,
              ),
              child: Align(
                alignment: Alignment.centerLeft,
                child: Text(
                  'Attach',
                  style: text.titleLarge?.copyWith(fontWeight: FontWeight.w700),
                ),
              ),
            ),
            _AttachOption(
              icon: Icons.insert_drive_file_rounded,
              label: 'Document',
              onTap: () => Navigator.pop(ctx, AttachKind.any),
            ),
            _AttachOption(
              icon: Icons.perm_media_rounded,
              label: 'Photos & videos',
              onTap: () => Navigator.pop(ctx, AttachKind.media),
            ),
            _AttachOption(
              icon: Icons.audiotrack_rounded,
              label: 'Audio',
              onTap: () => Navigator.pop(ctx, AttachKind.audio),
            ),
            const Gap(AppSpace.xs),
          ],
        ),
      );
    },
  );
}

/// One row in the attach menu.
class _AttachOption extends StatelessWidget {
  final IconData icon;
  final String label;
  final VoidCallback onTap;
  const _AttachOption({
    required this.icon,
    required this.label,
    required this.onTap,
  });

  @override
  Widget build(BuildContext context) {
    return ListTile(
      leading: Icon(icon, color: Theme.of(context).colorScheme.primary),
      title: Text(label),
      onTap: onTap,
    );
  }
}

/// The bottom compose bar: attach, a text field, and a send button.
///
/// [enabled] is false only for a peer with no known address, where the engine
/// would refuse the send before enqueueing it. The bar stays visible (a
/// vanishing composer reads as a broken screen) but every control is inert, so
/// nothing can be typed into a message that could not exist.
class _Composer extends StatelessWidget {
  final TextEditingController controller;
  final VoidCallback onSend;
  final VoidCallback onAttach;
  final bool enabled;
  const _Composer({
    required this.controller,
    required this.onSend,
    required this.onAttach,
    this.enabled = true,
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
              onPressed: enabled ? onAttach : null,
              icon: const Icon(Icons.attach_file_rounded),
              tooltip: 'Attach files',
            ),
            const Gap(AppSpace.xxs),
            Expanded(
              child: TextField(
                controller: controller,
                enabled: enabled,
                textInputAction: TextInputAction.send,
                minLines: 1,
                maxLines: 5,
                decoration: InputDecoration(
                  hintText: enabled ? 'Message' : 'Not reachable right now',
                  border: const OutlineInputBorder(
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
              onPressed: enabled ? onSend : null,
              icon: const Icon(Icons.send_rounded),
              tooltip: 'Send',
            ),
          ],
        ),
      ),
    );
  }
}
