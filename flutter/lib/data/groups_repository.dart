import 'package:flutter/foundation.dart';

import '../sdk/error_text.dart';
import '../sdk/models.dart';
import '../sdk/peerbeam.dart';

/// Groups and the invitations waiting for an answer, read through the engine.
///
/// Holds no roster of its own beyond the last read: the engine owns membership,
/// and a second copy here would be one more thing that can disagree with it —
/// the same rule [NotesRepository] follows.
///
/// # Why failures are returned rather than swallowed
///
/// Every write here is visible to **other people**: an invitation reaches
/// somebody's device, a send reaches everybody's, and leaving tells a room you
/// have gone. A refused write that looked like a successful one would leave the
/// user believing they had done something to other people that they had not,
/// which is a worse mistake than the equivalent in a private feature. So each
/// returns a sentence to show, and the screens show it.
class GroupsRepository extends ChangeNotifier {
  final PeerBeamApi? _api;

  GroupsRepository({PeerBeamApi? api})
    // ignore: prefer_initializing_formals
    : _api = api;

  List<Group> groups = const [];

  /// Offers, not memberships. Kept separate from [groups] because holding an
  /// invitation is not being in a group, and a list that mixed them would show
  /// a group this device has not joined.
  List<GroupInvite> invites = const [];

  /// True once a load has completed, so an empty list can be told apart from
  /// "not loaded yet" — otherwise the empty state flashes on every open.
  bool loaded = false;

  /// The last read's failure, or null. Distinct from an empty list: one means
  /// there are no groups, the other means we do not know.
  Object? error;

  Future<void> refresh() async {
    final api = _api;
    if (api == null) {
      loaded = true;
      notifyListeners();
      return;
    }
    try {
      final view = await api.groups();
      groups = view.groups;
      invites = view.invites;
      error = null;
    } catch (e) {
      // A failed read leaves the previous lists standing rather than blanking
      // the screen; `error` is what lets a surface say so instead of showing an
      // empty state that would read as "you are in no groups".
      error = e;
    }
    loaded = true;
    notifyListeners();
  }

  /// Create a group holding only this device.
  ///
  /// Returns null on success, or a sentence to show. Members are invited and
  /// join when **they** accept — there is deliberately no way to create a group
  /// with somebody else's device already in it.
  Future<String?> create(String name) async {
    final api = _api;
    if (api == null) return 'PeerBeam is not running.';
    try {
      await api.createGroup(name);
      await refresh();
      return null;
    } catch (e) {
      return friendlyError(e);
    }
  }

  /// Rename a group **on this device only**.
  Future<String?> rename(String id, String name) async {
    final api = _api;
    if (api == null) return 'PeerBeam is not running.';
    try {
      await api.renameGroup(id, name);
      await refresh();
      return null;
    } catch (e) {
      return friendlyError(e);
    }
  }

  /// Turn an invitation down. Local and silent — the inviter is not told.
  Future<String?> decline(String group) async {
    final api = _api;
    if (api == null) return 'PeerBeam is not running.';
    try {
      await api.declineGroupInvite(group);
      await refresh();
      return null;
    } catch (e) {
      return friendlyError(e);
    }
  }

  /// Offer [peer] a place in [id].
  ///
  /// The caller must have told the user what this discloses **before** calling:
  /// the invitee sees who is already in the group, and everyone in it sees them
  /// if they accept.
  Future<String?> invite(String id, PeerTarget peer) async {
    final api = _api;
    if (api == null) return 'PeerBeam is not running.';
    try {
      await api.inviteToGroup(id, peer);
      return null;
    } catch (e) {
      return friendlyError(e);
    }
  }

  /// Accept an invitation, telling every member the app can currently reach.
  ///
  /// [peers] comes from the device list the app already has — the engine holds
  /// ids, not routes. A member that cannot be reached now is told at next
  /// contact rather than blocking the join.
  Future<String?> accept(String group, List<PeerTarget> peers) async {
    final api = _api;
    if (api == null) return 'PeerBeam is not running.';
    try {
      await api.acceptGroupInvite(group, peers);
      await refresh();
      return null;
    } catch (e) {
      return friendlyError(e);
    }
  }

  /// Leave a group. Forgotten here whether or not anyone heard.
  Future<String?> leave(String id, List<PeerTarget> peers) async {
    final api = _api;
    if (api == null) return 'PeerBeam is not running.';
    try {
      await api.leaveGroup(id, peers);
      await refresh();
      return null;
    } catch (e) {
      return friendlyError(e);
    }
  }

  /// Send [text] to every member this device may message.
  ///
  /// Returns the result on success so a screen can name who was skipped, or a
  /// sentence when the send itself failed. Members that cannot be messaged are
  /// **not** a failure — the message went to the rest, and saying so is more
  /// useful than an error that implies nothing was sent.
  Future<({GroupSendResult? result, String? error})> send(
    String id,
    String text,
  ) async {
    final api = _api;
    if (api == null) {
      return (result: null, error: 'PeerBeam is not running.');
    }
    try {
      return (result: await api.sendToGroup(id, text), error: null);
    } catch (e) {
      return (result: null, error: friendlyError(e));
    }
  }

  /// A group's messages, gathered across its members.
  Future<List<ChatMessage>> history(String group) async {
    final api = _api;
    if (api == null) return const [];
    try {
      return await api.groupHistory(group);
    } catch (_) {
      // A failed read shows nothing rather than throwing into a build: the
      // screen renders its own error state from `error` when the list read
      // failed, and a thread that cannot be read is not a crash.
      return const [];
    }
  }
}
