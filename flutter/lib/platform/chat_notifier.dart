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
import '../sdk/models.dart';
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
  /// a thread can withdraw exactly its own. Keyed by [chatThreadKey], so a
  /// group and a private thread with the same sender are tracked apart.
  ///
  /// The value is the id of the **file offer** the notice is asking about, or
  /// null for an ordinary message. The notice's own id is a pure function of
  /// the key ([chatNoticeId]) and never needs remembering; what does is which
  /// question is still open. A file offer's notice asks something that gets
  /// answered elsewhere — from the incoming-transfer prompt, or automatically
  /// for a trusted device — and a question that has been answered must stop
  /// being asked. Left standing, "Wants to send report.pdf" sat there
  /// indefinitely, on Android directly above a second notification saying the
  /// same file had been received.
  final Map<String, String?> _standing = <String, String?>{};

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
    if (event is ChatStatus) {
      _onStatus(event);
      return;
    }
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
    // Remember the offer's id only while it is actually a question.
    _standing[chatThreadKey(message)] = message.awaitingApproval
        ? message.id
        : null;
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
    if (!_standing.containsKey(open)) return;
    _standing.remove(open);
    unawaited(withdraw(chatNoticeId(open)));
  }

  /// A file offer the standing notice was asking about has been settled.
  ///
  /// Accepted, declined, auto-accepted, failed — any of them means the question
  /// is no longer open, and the notice is the only thing still asking it.
  /// Withdrawn rather than reworded: what actually happened to the file is
  /// reported by the transfer notification that follows, and two notifications
  /// narrating one file is what this is reducing, not adding to.
  void _onStatus(ChatStatus status) {
    final key = status.peerId;
    if (!_standing.containsKey(key)) return;
    if (_standing[key] != status.messageId) return;
    if (status.status == ChatStatusValue.pendingApproval) return;
    _standing.remove(key);
    unawaited(withdraw(chatNoticeId(key)));
  }

  void dispose() {
    presence.removeListener(_onPresence);
    _sub?.cancel();
    _sub = null;
  }
}
