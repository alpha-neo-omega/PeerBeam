import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../data/trust_repository.dart';
import '../../sdk/error_text.dart';
import '../../sdk/exceptions.dart';
import '../../sdk/models.dart' show Space;
import '../../sdk/peerbeam.dart';
import '../../state/app_scope.dart';
import '../../widgets/appear.dart';
import '../../widgets/common.dart';
import 'add_space_device.dart';
import 'space_card.dart';
import 'space_name_dialog.dart';

/// Spaces: names this device keeps for sets of devices it already trusts.
///
/// # What this screen must never imply
///
/// A Space is **local**. It has no roster on the wire, no group id, no group
/// key and no membership messages: two devices in one Space have no idea that
/// they are, and no device is told when one is created, renamed, filled,
/// emptied or deleted. So there is no invite here, nothing to join or leave,
/// nobody who "can see" anything, and no room. Every word on this screen is
/// chosen against that — group language would describe a feature that does not
/// exist and cannot, since a device brokering membership on everyone else's
/// behalf is a hub, which is what `docs/SPACES.md` and invariant I3 rule out.
///
/// A Space is also **not a permission**. Adding a device grants it nothing:
/// each send a fan-out performs passes the same per-capability gate a
/// hand-addressed send would. A list of devices under a name the user typed is
/// exactly the thing that would otherwise read like an access list, so the copy
/// says so where it can be misread.
///
/// # Why every write is followed by a re-read
///
/// Membership is reconciled against the trust store on **every read** — a
/// device revoked, or a time-limited grant that ran out, moves from live to
/// stale with nothing writing to the Space. So the engine's answer is the only
/// thing that knows what a Space looks like, and this screen never adopts a
/// list it composed itself. That is also why a trust change refreshes it: a
/// revoke in Settings changes what belongs on this screen, and a device sitting
/// here looking reachable when a send would skip it is the failure this whole
/// feature is shaped to prevent.
///
/// Sending is deliberately absent. The fan-out lives in the CLI, and a button
/// here that did nothing — or that reached some devices and said nothing about
/// the rest — would be worse than the honest gap the copy names.
class SpacesScreen extends StatefulWidget {
  const SpacesScreen({super.key});

  @override
  State<SpacesScreen> createState() => _SpacesScreenState();
}

class _SpacesScreenState extends State<SpacesScreen> {
  List<Space> _spaces = const [];
  bool _loaded = false;

  /// The last read's failure, or null. An absence and a failure must not render
  /// the same: "No Spaces yet" is a claim about the engine's list, and stating
  /// it when no list came back tells someone they have none when they may have
  /// several.
  Object? _error;

  /// Held so the listener can be detached: the screen lives in an indexed stack
  /// and is never rebuilt, so a listener left behind would outlive it.
  TrustRepository? _trust;

  @override
  void initState() {
    super.initState();
    // After the first frame, like every other screen: the engine may not have
    // finished `initialize()` while this is being constructed.
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      _trust = AppScope.of(context).trust..addListener(_onTrustChanged);
      _load();
    });
  }

  @override
  void dispose() {
    _trust?.removeListener(_onTrustChanged);
    super.dispose();
  }

  /// A pin was revoked, granted or re-read, so the live/stale split every card
  /// renders may have changed under it. Re-read rather than repaint: the
  /// partition is the engine's, made against the trust store, and this screen
  /// has no business guessing it.
  void _onTrustChanged() => _load();

  Future<void> _load() async {
    final api = AppScope.of(context).api;
    try {
      final spaces = api == null ? <Space>[] : await api.spaces();
      if (!mounted) return;
      setState(() {
        _spaces = spaces;
        _error = null;
        _loaded = true;
      });
    } catch (e) {
      if (!mounted) return;
      setState(() {
        _error = e;
        _loaded = true;
      });
    }
  }

  // ---------------------------------------------------------------- writes

  Future<void> _create() async {
    final name = await showSpaceNameDialog(context);
    if (name == null || !mounted) return;
    final api = AppScope.of(context).api;
    final messenger = ScaffoldMessenger.of(context);
    if (api == null) return;
    await _run(
      messenger,
      () => api.createSpace(name),
      failure: 'Could not create “$name”',
    );
  }

  Future<void> _rename(Space space) async {
    final name = await showSpaceNameDialog(context, current: space.name);
    if (name == null || !mounted) return;
    final api = AppScope.of(context).api;
    final messenger = ScaffoldMessenger.of(context);
    if (api == null) return;
    await _run(
      messenger,
      () => api.renameSpace(space.id, name),
      failure: 'Could not rename “${space.name}”',
    );
  }

  /// Delete a Space, with an undo.
  ///
  /// An undo rather than a confirmation, following the save rules and the
  /// shared folders: a Space is fully described by the card that was showing it
  /// — a name and a list of device ids, all of them on screen — so putting it
  /// back is exact, and a dialog in front of a delete that is this cheap to
  /// reverse only trains people to dismiss dialogs. Nothing is destroyed
  /// elsewhere either: the devices keep their trust, and no peer ever knew the
  /// Space existed.
  Future<void> _delete(Space space) async {
    final api = AppScope.of(context).api;
    final messenger = ScaffoldMessenger.of(context);
    if (api == null) return;
    var deleted = false;
    try {
      deleted = await api.deleteSpace(space.id);
    } catch (e) {
      _say(messenger, 'Could not delete “${space.name}” — ${_reason(e)}');
    }
    await _load();
    // Only offer to take back what actually happened — a refused delete has
    // reported itself, and an Undo beside it would be undoing nothing. A false
    // return is the Space already being gone, which is worth saying rather than
    // leaving the row's disappearance to explain itself.
    if (!deleted) return;
    _say(
      messenger,
      'Deleted “${space.name}”',
      action: SnackBarAction(
        label: 'Undo',
        onPressed: () => _restore(space, api, messenger),
      ),
    );
  }

  /// Put a deleted Space back from the record that was on screen.
  ///
  /// The id is not restored: it is opaque, never shown, and a fresh one costs
  /// nothing — what the user named and filled is the name and the devices.
  ///
  /// Every device is attempted, stale ones included, and the refusals are
  /// counted rather than assumed. The engine will not add a device it holds no
  /// live pin for, so a stale one usually cannot go back — but a device that
  /// was stale a minute ago may have been re-paired since, and only the engine
  /// knows. What must not happen is a restore that quietly comes back short:
  /// silence there would leave someone believing a Space holds a device it does
  /// not, which is the same wrong belief the stale rows exist to prevent.
  Future<void> _restore(
    Space space,
    PeerBeamApi api,
    ScaffoldMessengerState messenger,
  ) async {
    final Space recreated;
    try {
      recreated = await api.createSpace(space.name);
    } catch (e) {
      _say(messenger, 'Could not put “${space.name}” back — ${_reason(e)}');
      await _load();
      return;
    }
    var refused = 0;
    for (final device in [...space.live, ...space.stale]) {
      try {
        await api.addSpaceMember(recreated.id, device);
      } catch (_) {
        refused++;
      }
    }
    await _load();
    if (refused == 0) return;
    _say(
      messenger,
      refused == 1
          ? 'Put “${space.name}” back without 1 device this machine no longer '
                'trusts.'
          : 'Put “${space.name}” back without $refused devices this machine no '
                'longer trusts.',
    );
  }

  Future<void> _addDevice(Space space) async {
    final chosen = await showSpaceDevicePicker(
      context,
      spaceName: space.name,
      // Live and stale both: a device already in the Space has a row on screen,
      // and offering it again could only add nothing or be refused.
      already: {...space.live, ...space.stale},
    );
    if (chosen == null || !mounted) return;
    final api = AppScope.of(context).api;
    final messenger = ScaffoldMessenger.of(context);
    if (api == null) return;
    // No snackbar on success: the row appearing under the Space is the
    // confirmation, and it says more than a sentence could — including whether
    // the engine considers the device live.
    await _run(
      messenger,
      () => api.addSpaceMember(space.id, chosen.id),
      failure:
          'Could not add ${chosen.name.isEmpty ? chosen.id : chosen.name} to '
          '“${space.name}”',
    );
  }

  /// Take one device out of a Space.
  ///
  /// Removal validates nothing and is allowed to succeed for a device this
  /// machine no longer trusts — that is precisely the device you most need to
  /// be able to take out. The undo, though, is only offered for a live one: the
  /// engine refuses to add a device it holds no live pin for, so an Undo on a
  /// stale row would fail the moment it was pressed. It says why instead.
  Future<void> _removeDevice(
    Space space,
    String device, {
    required bool stale,
  }) async {
    final api = AppScope.of(context).api;
    final messenger = ScaffoldMessenger.of(context);
    if (api == null) return;
    final label = _nameOf(device);
    var removed = false;
    try {
      removed = await api.removeSpaceMember(space.id, device);
    } catch (e) {
      _say(
        messenger,
        'Could not take $label out of “${space.name}” — ${_reason(e)}',
      );
    }
    await _load();
    if (!removed) return;
    if (stale) {
      _say(
        messenger,
        'Took $label out of “${space.name}”. It can go back in once this '
        'device trusts it again.',
      );
      return;
    }
    _say(
      messenger,
      'Took $label out of “${space.name}”',
      action: SnackBarAction(
        label: 'Undo',
        onPressed: () => _run(
          messenger,
          () => api.addSpaceMember(space.id, device),
          failure: 'Could not put $label back',
        ),
      ),
    );
  }

  /// One engine write, its refusal surfaced, then a re-read of what is actually
  /// stored. Returns whether the write landed.
  Future<bool> _run(
    ScaffoldMessengerState messenger,
    Future<void> Function() write, {
    required String failure,
  }) async {
    var ok = true;
    try {
      await write();
    } catch (e) {
      ok = false;
      _say(messenger, '$failure — ${_reason(e)}');
    }
    await _load();
    return ok;
  }

  void _say(
    ScaffoldMessengerState messenger,
    String message, {
    SnackBarAction? action,
  }) => messenger
    ..hideCurrentSnackBar()
    ..showSnackBar(SnackBar(content: Text(message), action: action));

  /// A refused write carries the engine's own reason — which name rule was
  /// broken, or that a device is not trusted — and that is far more use than
  /// the generic "that action can't be completed". Anything else falls back to
  /// the shared friendly text.
  static String _reason(Object e) =>
      e is InvalidArgumentException ? e.message : friendlyError(e);

  /// A device id as something a person recognises, falling back to the id.
  ///
  /// Discovery first, then trust, as everywhere else. A stale device commonly
  /// resolves to neither — the trust record that held its name is the one that
  /// went away — and the id is then the honest answer rather than a name this
  /// screen invented.
  String _nameOf(String deviceId) {
    final state = AppScope.of(context);
    for (final d in state.device.devices) {
      if (d.id == deviceId) return d.name;
    }
    for (final t in state.trust.items) {
      if (t.id == deviceId && t.name.isNotEmpty) return t.name;
    }
    return deviceId;
  }

  // ---------------------------------------------------------------- build

  @override
  Widget build(BuildContext context) {
    final state = AppScope.of(context);
    final theme = Theme.of(context);
    return Scaffold(
      appBar: AppBar(
        title: const Text('Spaces'),
        actions: [
          IconButton(
            icon: const Icon(Icons.refresh_rounded),
            tooltip: 'Refresh',
            onPressed: _load,
          ),
        ],
      ),
      floatingActionButton: FloatingActionButton(
        onPressed: _create,
        tooltip: 'New Space',
        child: const Icon(Icons.add_rounded),
      ),
      body: SafeArea(
        child: ContentPane(
          child: Column(
            children: [
              _note(theme),
              Expanded(
                // Discovery and trust are listened to as well, because they
                // supply the *name* a device row renders: a peer coming online
                // must relabel its row without the user leaving and coming
                // back.
                child: AnimatedBuilder(
                  animation: Listenable.merge([state.device, state.trust]),
                  builder: (context, _) => _body(),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }

  /// The two facts the whole screen rests on, stated where they cannot be
  /// missed and kept visible in every state — including while the first read is
  /// still out, so the frame is never blank and never silent.
  Widget _note(ThemeData theme) => Padding(
    padding: const EdgeInsets.fromLTRB(
      AppSpace.md,
      AppSpace.sm,
      AppSpace.md,
      AppSpace.xs,
    ),
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          'A Space is a name this device keeps for devices you already trust. '
          'Nothing about it leaves this machine: no device is told it is in '
          'one, and no device can see the others in it.',
          style: theme.textTheme.bodySmall?.copyWith(
            color: theme.colorScheme.onSurfaceVariant,
          ),
        ),
        const Gap(AppSpace.xxs),
        Text(
          // The honest gap, named rather than papered over with a button. The
          // first half is the part that has to survive: being in a Space is not
          // a permission, and every send is still checked per device.
          'Sending to a Space is one ordinary send per device, checked against '
          'that device’s own permissions — being in a Space grants nothing. '
          'The fan-out runs in the command line today (“peerbeam space send”), '
          'over these same Spaces; there is no send button here yet.',
          style: theme.textTheme.bodySmall?.copyWith(
            color: theme.colorScheme.onSurfaceVariant,
          ),
        ),
      ],
    ),
  );

  /// Centre a state in what is left of the screen, and let it scroll when that
  /// is not enough.
  ///
  /// [EmptyState] and [ErrorState] both centre a `Column` sized to its content,
  /// which overflows rather than scrolls the moment the content is taller than
  /// the space it was given. On a 360×720 phone the explanation above this had
  /// already taken enough of the column that the empty state overflowed by a
  /// few pixels — and an overflow is the one way for a state whose whole job is
  /// to explain something to explain nothing.
  Widget _fitted(Widget state) => LayoutBuilder(
    builder: (context, constraints) => SingleChildScrollView(
      child: ConstrainedBox(
        constraints: BoxConstraints(minHeight: constraints.maxHeight),
        child: state,
      ),
    ),
  );

  Widget _body() {
    if (!_loaded) {
      // Not a blank frame: the note above is already on screen, and this says
      // the list is on its way rather than that there is nothing in it.
      return _fitted(const Center(child: CircularProgressIndicator()));
    }
    if (_error != null) {
      return _fitted(
        ErrorState(
          error: _error!,
          title: 'Could not read your Spaces',
          onRetry: _load,
        ),
      );
    }
    if (_spaces.isEmpty) {
      return _fitted(
        EmptyState(
          icon: Icons.workspaces_outlined,
          title: 'No Spaces yet',
          message:
              'Name a set of devices you already trust and it will be kept '
              'here, on this device only.',
          action: FilledButton.tonalIcon(
            onPressed: _create,
            icon: const Icon(Icons.add_rounded),
            label: const Text('New Space'),
          ),
        ),
      );
    }
    return ListView.builder(
      // Room at the bottom for the floating button: without it the last card's
      // remove buttons sit underneath a button that creates a Space, and a
      // mis-tap there is one nobody expects to be reversible.
      padding: const EdgeInsets.fromLTRB(
        AppSpace.md,
        AppSpace.md,
        AppSpace.md,
        AppSpace.xxxl + AppSpace.xl,
      ),
      itemCount: _spaces.length,
      itemBuilder: (context, i) {
        final space = _spaces[i];
        return Appear(
          index: i,
          child: Padding(
            padding: const EdgeInsets.only(bottom: AppSpace.xs),
            child: SpaceCard(
              space: space,
              nameOf: _nameOf,
              onRename: () => _rename(space),
              onDelete: () => _delete(space),
              onAddDevice: () => _addDevice(space),
              onRemoveDevice: (device, {required stale}) =>
                  _removeDevice(space, device, stale: stale),
            ),
          ),
        );
      },
    );
  }
}
