/// What a notification for an arriving message should say, and whether to
/// raise one at all.
///
/// **Pure, and separate from the thing that shows it.** Delivery differs per
/// platform — Android has the foreground-service bridge, desktop has a plugin,
/// a widget test has neither — but the rules are the same everywhere and they
/// are the part with judgement in them: what counts as worth interrupting
/// someone for, and what a message may say on a lock screen. Those get tests;
/// the plugin call site cannot be driven headless.
library;

import '../sdk/models.dart';

/// Identifies the conversation a message belongs to.
///
/// A one-to-one thread is keyed by the peer's device id, which is what the
/// chat store and every chat surface already key by. A group is keyed by its
/// id behind a `group:` prefix.
///
/// The two cannot collide, and not by luck: the engine's store refuses a
/// namespace containing a colon (`namespace_dir`, `[A-Za-z0-9._-]` only), so
/// no device id this app can hold a conversation with contains one. That is
/// the same rule that makes a Tailscale node id (`ts:<node>`) unusable as a
/// chat key — see `peerbeam identify`.
String chatThreadKey(ChatMessage message) {
  final group = message.group;
  return group == null ? message.peerId : 'group:$group';
}

/// The presence key for a group's transcript, for a screen that has the group
/// rather than a message from it.
String groupThreadKey(String groupId) => 'group:$groupId';

/// What to show for one arriving message.
class ChatNotice {
  /// Stable per conversation, so a second message in the same thread replaces
  /// the first rather than stacking. Someone who stepped away comes back to one
  /// notification per conversation, not forty.
  final int id;

  /// The conversation's name — the peer's, or the group's. Never a device id
  /// unless there is nothing else to call it.
  final String title;

  /// The message, or a description of what arrived. In a group it is prefixed
  /// with who said it, which is the thing a group notification must not omit.
  final String body;

  /// The conversation to open when it is clicked. See [chatThreadKey].
  final String threadKey;

  const ChatNotice({
    required this.id,
    required this.title,
    required this.body,
    required this.threadKey,
  });

  @override
  bool operator ==(Object other) =>
      other is ChatNotice &&
      other.id == id &&
      other.title == title &&
      other.body == body &&
      other.threadKey == threadKey;

  @override
  int get hashCode => Object.hash(id, title, body, threadKey);

  @override
  String toString() => 'ChatNotice($id, $title, $body, $threadKey)';
}

/// The longest preview shown, in characters.
///
/// A notification is rendered outside the app — on a lock screen, in a
/// notification centre, over whatever the person is doing — so it carries as
/// little of the message as still makes it recognisable. The whole text would
/// put a private conversation somewhere the app cannot take it back from.
const int kChatPreviewChars = 120;

/// Whether an arriving message should raise a notification at all.
///
/// Three reasons not to, and each is a case where a notification would be
/// telling someone what they can already see, or what they did themselves:
///
/// * **Our own message.** An outgoing row echoes back through the same event.
/// * **The conversation is already on screen** *and* the window has focus. Not
///   one or the other: a thread open behind a locked screen or another app is
///   exactly the case a notification is for.
/// * **A status change rather than an arrival** — a row that settled from
///   `pending` to `sent` is not news. (`sent` is the only settled-outbound
///   status a record can carry; there is no separate delivered state.)
///
/// [openConversation] is a [chatThreadKey], so a group row is compared against
/// the group's transcript and never against the sender's private thread. Both
/// kinds of conversation notify; filing one under the other is the mistake to
/// avoid, and the key is what avoids it.
bool shouldNotifyForMessage(
  ChatMessage message, {
  required String? openConversation,
  required bool windowFocused,
}) {
  if (message.isMine) return false;
  if (message.status == ChatStatusValue.sent) return false;
  final watching = windowFocused && openConversation == chatThreadKey(message);
  return !watching;
}

/// The notification id for a conversation, from its [chatThreadKey].
///
/// Derived from the thread, so every message in one conversation reuses it: a
/// notification is *replaced* rather than stacked, and someone who stepped away
/// comes back to one line per conversation instead of forty. It is also what
/// withdraws the notice once the thread is opened.
///
/// Kept in its own band, `0x20000000`-`0x2FFFFFFF`, clear of the service
/// notification (id 1) and of received-file ids (`0x40000000` and up, see
/// `TransferNotifications`). `TransferNotifications.idFor` hashes across the
/// whole positive range and so could in principle land here; at one chance in
/// 2^28 per transfer, and with a collision costing one replaced notification,
/// that is accepted rather than engineered around.
int chatNoticeId(String threadKey) =>
    0x20000000 + (threadKey.hashCode & 0x0FFFFFFF);

/// The notice for [message], assuming [shouldNotifyForMessage] said yes.
///
/// [peerName] is what to call the sender and [groupName] what to call the
/// group. Either being empty falls back to the id — ugly but true; inventing a
/// friendly name for something we cannot name would be worse.
ChatNotice chatNotice(
  ChatMessage message, {
  required String peerName,
  String groupName = '',
}) {
  final from = peerName.trim().isEmpty ? message.peerId : peerName.trim();
  final group = message.group;
  final key = chatThreadKey(message);
  if (group == null) {
    return ChatNotice(
      id: chatNoticeId(key),
      title: from,
      body: _preview(message),
      threadKey: key,
    );
  }
  return ChatNotice(
    id: chatNoticeId(key),
    title: groupName.trim().isEmpty ? group : groupName.trim(),
    // Who spoke, then what they said. In a one-to-one thread the title already
    // answers "who"; in a group it answers "which conversation", and a body
    // without the speaker leaves the one question a group message raises
    // unanswered.
    body: '$from: ${_preview(message)}',
    threadKey: key,
  );
}

/// What the notification says arrived.
///
/// A file is named rather than previewed — the name is what the person needs to
/// decide whether to look now — and a file still awaiting a decision says so,
/// because that one is a question rather than an arrival.
String _preview(ChatMessage message) {
  if (message.isFile) {
    final name = (message.fileName ?? '').trim();
    final what = name.isEmpty ? 'a file' : name;
    return message.awaitingApproval ? 'Wants to send $what' : what;
  }
  final body = message.body.trim();
  if (body.isEmpty) return 'New message';
  // Collapse newlines: a notification is one line in most places, and a
  // multi-line body is rendered with the breaks stripped anyway — doing it here
  // means the truncation below counts what will actually be shown.
  final flat = body.replaceAll(RegExp(r'\s+'), ' ');
  if (flat.length <= kChatPreviewChars) return flat;
  return '${flat.substring(0, kChatPreviewChars).trimRight()}…';
}
