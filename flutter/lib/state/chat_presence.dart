/// Where the user's attention is, as far as chat is concerned.
///
/// Two facts, and only these two: which conversation is on screen, and whether
/// the app has the foreground at all. Together they answer the one question a
/// notification has to ask before it interrupts anyone — *can they already see
/// this?*
///
/// **Local, and never persisted or sent.** Nothing here reaches the engine, the
/// disk or a peer. It is not a read receipt (PeerBeam's are opt-in and live in
/// the engine, see `SettingsStore.readReceipts`) and it is not an unread count
/// — `ChatConversation.unreadHint` explains at length why this app refuses to
/// claim one. It is this process's own knowledge about its own window, alive
/// for exactly as long as the process is.
library;

import 'package:flutter/foundation.dart';

/// Tracks the open thread and the app's foreground state.
///
/// A [ChangeNotifier] so a notifier can react to a thread being opened — that
/// is the moment a notification about it becomes stale and should come down.
class ChatPresence extends ChangeNotifier {
  /// The threads on screen, in the order they were opened; the last is the one
  /// in front.
  ///
  /// **A stack, not a single slot**, because chat screens nest: opening a
  /// thread from inside another pushes a route over it, and popping back
  /// reveals the first again. A single slot got the push right and the pop
  /// wrong — the popped screen's `dispose` cleared the slot, and the thread the
  /// user was returned to then had nothing saying it was on screen, so every
  /// message arriving in the conversation they were reading raised a
  /// notification about it.
  ///
  /// It also makes the ordering safe by construction. Flutter builds the
  /// incoming route before disposing the outgoing one, so `enter` and `leave`
  /// arrive interleaved; removing by value cannot disturb an entry that is not
  /// its own.
  final List<String> _stack = <String>[];

  bool _foreground = true;

  /// The peer whose thread is in front, or null when none is.
  String? get openConversation => _stack.isEmpty ? null : _stack.last;

  /// Whether the app is frontmost. Starts true: the app has just been
  /// launched, so it is.
  bool get foreground => _foreground;

  /// Whether the user can plainly see messages arriving from [peerId] right
  /// now. Both halves must hold — a thread left open behind a locked screen or
  /// another window is precisely the case a notification exists for.
  bool isWatching(String peerId) => _foreground && openConversation == peerId;

  /// A thread was opened.
  ///
  /// A key appears at most once. Without that, a screen that registered while
  /// buried under another — `didChangeDependencies` fires for an ordinary
  /// theme change, not only on the way in — pushed a **second** copy of
  /// itself, and `leave` removes one occurrence. The leftover entry then sat on
  /// the stack for the rest of the session claiming that thread was on screen,
  /// so messages arriving in it stopped raising notifications with nothing
  /// open at all.
  ///
  /// The screens also no longer re-enter on every dependency change (see
  /// `ChatScreen.didChangeDependencies`); this is the invariant holding
  /// regardless.
  void enter(String peerId) {
    if (openConversation == peerId) return;
    _stack
      ..remove(peerId)
      ..add(peerId);
    notifyListeners();
  }

  /// A thread was left.
  ///
  /// Removes that thread's own entry wherever it sits, and only notifies when
  /// the thread in front actually changed — leaving a screen buried under
  /// another is not a change of attention.
  void leave(String peerId) {
    final was = openConversation;
    if (!_stack.remove(peerId)) return;
    if (openConversation != was) notifyListeners();
  }

  /// The app gained or lost the foreground.
  void setForeground(bool value) {
    if (_foreground == value) return;
    _foreground = value;
    notifyListeners();
  }
}
