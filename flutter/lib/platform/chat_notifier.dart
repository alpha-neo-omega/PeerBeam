/// Turns arriving messages into notifications.
///
/// Three inputs, all injected: the engine's event stream, where the user's
/// attention is ([ChatPresence]), and a way to post and withdraw a notice. The
/// last of those is per-platform — Android's own bridge, the desktop plugin, or
/// nothing at all in a widget test — and this class does not know which it has.
///
/// Nothing here is a capability. A notification says only that a message
/// arrived and from whom; it grants nothing, sends nothing, and is not
/// reachable by a peer. The engine remains the only thing that decides what
/// gets delivered, so a GUI-only notifier does not put a capability out of the
/// CLI's reach (invariant I7).
library;

import 'dart:async';

import '../sdk/events.dart';
import '../state/chat_presence.dart';
import 'chat_notifications.dart';

/// Watches for incoming messages and notifies about the ones the user cannot
/// already see.
class ChatNotifier {
  /// The engine's event stream, or null when there is no engine (widget tests
  /// that build state without one). Null makes [start] a no-op.
  final Stream<BridgeEvent>? events;

  final ChatPresence presence;

  /// The user's "Notifications" setting, read live rather than captured: a
  /// switch turned off has to take effect on the next message, not the next
  /// launch.
  final bool Function() enabled;

  /// What to call a peer. Falls back to the device id inside [chatNotice] when
  /// this returns empty.
  final String Function(String peerId) nameOf;

  /// What to call a group. Same fallback.
  final String Function(String groupId) groupNameOf;

  /// Show a notification. Per-platform; see [ChatNotice].
  final Future<void> Function(ChatNotice notice) post;

  /// Take one down again, by id.
  final Future<void> Function(int id) withdraw;

  StreamSubscription<BridgeEvent>? _sub;

  /// The conversations that currently have a notification standing, so opening
  /// a thread can withdraw exactly its own. Holds [chatThreadKey]s, so a group
  /// and a private thread with the same sender are tracked apart.
  ///
  /// A set rather than a map: the id is a pure function of the key
  /// ([chatNoticeId]), so there is nothing else to remember, and no way for a
  /// remembered id to drift from the one that was posted.
  final Set<String> _standing = <String>{};

  ChatNotifier({
    required this.events,
    required this.presence,
    required this.enabled,
    required this.nameOf,
    required this.groupNameOf,
    required this.post,
    required this.withdraw,
  });

  /// Begin watching. Safe to call when [events] is null.
  void start() {
    _sub ??= events?.listen(_onEvent);
    presence.addListener(_onPresence);
  }

  void _onEvent(BridgeEvent event) {
    if (event is! ChatReceived) return;
    final message = event.message;
    // The setting is checked here rather than inside the pure predicate: it is
    // a preference about this app, not a fact about the message, and the
    // predicate is about the message.
    if (!enabled()) return;
    if (!shouldNotifyForMessage(
      message,
      openConversation: presence.openConversation,
      windowFocused: presence.foreground,
    )) {
      return;
    }
    final group = message.group;
    _standing.add(chatThreadKey(message));
    unawaited(
      post(
        chatNotice(
          message,
          peerName: nameOf(message.peerId),
          groupName: group == null ? '' : groupNameOf(group),
        ),
      ),
    );
  }

  /// Attention moved: withdraw the notice for a thread the user is now looking
  /// at.
  ///
  /// Fires both when a thread is opened and when the app returns to the
  /// foreground with one already open — in either case the notice has been
  /// answered, and leaving it in the notification centre would ask again for
  /// something already done.
  void _onPresence() {
    final open = presence.openConversation;
    if (open == null || !presence.foreground) return;
    if (!_standing.remove(open)) return;
    unawaited(withdraw(chatNoticeId(open)));
  }

  void dispose() {
    presence.removeListener(_onPresence);
    _sub?.cancel();
    _sub = null;
  }
}
