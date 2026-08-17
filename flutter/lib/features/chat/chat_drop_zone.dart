import 'dart:async';

import 'package:desktop_drop/desktop_drop.dart';
import 'package:flutter/material.dart';

import '../../sdk/models.dart';
import '../../state/app_scope.dart';
import '../send/drop_overlay.dart';
import '../send/drop_zone.dart' show DropClaims, collectDroppedFiles, isDesktop;

/// Wraps [child] with desktop file drag & drop for one open conversation:
/// a drop sends straight to [peer], the same as picking files with attach.
///
/// On non-desktop platforms this is a transparent passthrough, exactly like
/// [DropZone] — `desktop_drop` is a desktop plugin.
///
/// Reuses [DropZone]'s own `collectDroppedFiles` for the `XFile` →
/// `StagedFile` walk (metadata only, folder detection, name fallback); the
/// only thing that differs from the Send flow is what happens with the
/// result — the Send flow opens the staged-files sheet, this sends straight
/// into the open conversation.
class ChatDropZone extends StatefulWidget {
  final String peerId;
  final PeerTarget peer;

  /// Whether this peer can be sent to at all — the composer's own `_canSend`.
  ///
  /// A peer with no known address is refused by the engine before anything is
  /// enqueued, which is why the composer disables its attach button rather
  /// than accepting files that would never exist. A drop is the same act by a
  /// different gesture, so it must answer the same way: dropping onto a
  /// conversation whose composer is disabled would otherwise fan out one
  /// doomed send per file and leave the user a row of failed bubbles to
  /// dismiss, having been told nothing up front.
  final bool canSend;
  final Widget child;
  const ChatDropZone({
    super.key,
    required this.peerId,
    required this.peer,
    required this.canSend,
    required this.child,
  });

  @override
  State<ChatDropZone> createState() => _ChatDropZoneState();
}

class _ChatDropZoneState extends State<ChatDropZone> {
  bool _active = false;

  /// The enclosing [DropZone]'s claim register.
  ///
  /// The shell wraps everything in a [DropZone], and `desktop_drop` delivers a
  /// drop to every mounted target rather than only the innermost — so without
  /// claiming, dropping on a conversation both sent the file to the peer and
  /// staged it for the Send flow. Claiming makes this zone the only one that
  /// answers while the conversation is on screen.
  ValueNotifier<int>? _register;

  /// The register this zone is actually counted in, or null while it holds no
  /// claim.
  ///
  /// The single source of truth for whether a decrement is owed, and never
  /// anything but [_register]: a separate boolean beside it could disagree with
  /// it, and the direction it disagrees in is either an outer zone left
  /// permanently deaf to drops or a count it can never clear.
  ValueNotifier<int>? _held;

  /// Whether this conversation is the one the user is actually looking at.
  bool _visible = true;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final register = DropClaims.maybeOf(context);
    if (!identical(register, _register)) {
      // A DIFFERENT register means the [DropZone] above was not merely rebuilt
      // but REPLACED. `AppShell` puts it in the `Scaffold`'s `body` below
      // `Breakpoints.compact` and inside a `Row` beside the navigation rail
      // above it, so dragging the window across 600px destroys
      // `_DropZoneState` and the notifier it owns, while the branch
      // Navigator's GlobalKey carries this State over into its replacement.
      //
      // The claim is therefore DROPPED, not released. Decrementing the old
      // register would be touching a `ValueNotifier` its owner has already
      // disposed — the "used after being disposed" throw — and the count it
      // would be correcting is going away with it either way. Claiming afresh
      // on the register that now exists is the whole of what is owed.
      _held = null;
      _register = register;
    }
    _visible = _onScreen();
    _reconcile();
  }

  /// Whether a drop made right now would land on *this* conversation.
  ///
  /// **Mounted is not the same as on screen**, and the gap between the two is
  /// exactly where a file goes to a peer the user never picked. The app shell
  /// is a `StatefulShellRoute.indexedStack`, which keeps every navigation
  /// branch mounted and merely takes the inactive ones offstage — and
  /// `desktop_drop`'s own gate is `renderBox.paintBounds.contains(…)`, which an
  /// offstage `IndexedStack` child passes because it is still laid out at full
  /// size. So a conversation left open on the Chats tab would answer a drop
  /// made on Home: the file is sent straight to that peer instead of being
  /// staged for the Send flow, with no prompt and no way back.
  ///
  /// Two signals, because there are two ways to stop being on screen:
  ///
  ///  * go_router wraps each branch in `Offstage(offstage: !isActive, child:
  ///    TickerMode(enabled: isActive, …))`, so [TickerMode] reads false for a
  ///    branch the user has navigated away from;
  ///  * a route pushed *on top of* this one inside the same branch leaves
  ///    `TickerMode` true, and is caught by [ModalRoute]'s `isCurrent` instead.
  ///
  /// Both are inherited-widget lookups, which is what makes them reactive:
  /// [didChangeDependencies] runs again the moment either changes. A null route
  /// counts as visible — a chat shown outside a Navigator (a test, or a future
  /// secondary window) has nothing on top of it.
  bool _onScreen() =>
      TickerMode.valuesOf(context).enabled &&
      (ModalRoute.of(context)?.isCurrent ?? true);

  /// Hold or release the claim so that it matches what this zone can see right
  /// now — given the register in scope and whether this conversation is on
  /// screen, either the claim is held or it is not.
  ///
  /// Deliberately one path and not two mechanisms, because two could disagree
  /// about who owns the next drop, and the answer to that question is which
  /// device a file leaves for.
  ///
  /// It only ever decrements [_held], which [didChangeDependencies] has already
  /// reconciled to the live register — so a stale one is dropped there rather
  /// than decremented here.
  ///
  /// Mutating the register during this screen's own build is deliberate and
  /// safe: [DropZone] listens to it and defers its own rebuild to after the
  /// frame, so nothing rebuilds mid-build.
  void _reconcile() {
    final wanted = _visible ? _register : null;
    if (identical(_held, wanted)) return;
    final held = _held;
    if (held != null) held.value--;
    _held = wanted;
    if (wanted != null) wanted.value++;
  }

  @override
  void dispose() {
    // Only if a claim was actually taken. [_held] is null whenever this zone is
    // not counted in a register, so this can neither hand the outer zone a
    // negative count it could never clear nor touch a notifier that has gone.
    final held = _held;
    if (held != null) held.value--;
    super.dispose();
  }

  /// A chat file message carries exactly one file, and the engine's
  /// `prepare_file_send` rejects a directory outright — so a dropped folder
  /// cannot go through this path at all. Every non-folder item is sent, one
  /// message each, exactly the fan-out `_attach` already uses for a
  /// multi-select; every folder is skipped and named together in a single
  /// message, rather than vanishing silently or surfacing as an engine error
  /// later.
  Future<void> _onDone(DropDoneDetails detail) async {
    setState(() => _active = false);
    // The second, independent guard on the same question `enable` answers, and
    // the reason it is worth stating twice: `desktop_drop` chooses which
    // targets to notify from paint bounds alone, which an offstage branch still
    // passes, so a claim that failed to be released must not on its own be
    // enough to make a conversation nobody is looking at answer a drop.
    if (!_visible) return;
    // Checked before the files are even walked: nothing here can succeed, and
    // saying so once beats staging metadata for a send that cannot be made.
    if (!widget.canSend) {
      if (!mounted) return;
      ScaffoldMessenger.of(context)
        ..hideCurrentSnackBar()
        ..showSnackBar(
          SnackBar(
            content: Text(
              'No address known for ${widget.peer.name} yet — files can be '
              'sent again as soon as the device is discovered.',
            ),
          ),
        );
      return;
    }
    final staged = await collectDroppedFiles(detail);
    if (!mounted) return;
    final chat = AppScope.of(context).chat;
    final folders = <String>[];
    for (final item in staged) {
      if (item.isDirectory) {
        folders.add(item.name);
        continue;
      }
      // Not awaited, and deliberately not sequential: each call appends its
      // own optimistic row synchronously, so a multi-file drop appears all at
      // once, the same as a multi-select attach does.
      unawaited(
        chat.sendFile(
          widget.peerId,
          widget.peer,
          item.path,
          name: item.name,
          size: item.size,
        ),
      );
    }
    if (folders.isEmpty || !mounted) return;
    // One message for every folder the drop skipped — a mixed drop must not
    // abort the files that DID go through, and must not fire one snackbar per
    // folder either.
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(_folderRefusal(folders))));
  }

  @override
  Widget build(BuildContext context) {
    if (!isDesktop) return widget.child;

    return DropTarget(
      // Off entirely while this conversation is offstage or buried under
      // another route — see [_onScreen].
      enable: _visible,
      onDragEntered: (_) => setState(() => _active = true),
      onDragExited: (_) => setState(() => _active = false),
      onDragDone: _onDone,
      child: Stack(
        fit: StackFit.expand,
        children: [
          widget.child,
          Positioned.fill(child: DropOverlay(active: _active)),
        ],
      ),
    );
  }
}

/// Names every folder a drop had to skip and points at the flow that can
/// actually send one — "Send folder" on Home — rather than leaving the user
/// to wonder why a folder they dropped never arrived.
String _folderRefusal(List<String> names) {
  final one = names.length == 1;
  final named = names.map((n) => '"$n"').join(', ');
  return '$named ${one ? 'is a folder' : 'are folders'} — a chat message can '
      'only carry one file at a time. Use "Send folder" from Home to send '
      '${one ? 'it' : 'them'}.';
}
