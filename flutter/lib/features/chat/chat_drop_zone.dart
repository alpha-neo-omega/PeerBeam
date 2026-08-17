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

  /// The enclosing [DropZone]'s claim register, and whether this zone is
  /// currently counted in it.
  ///
  /// The shell wraps everything in a [DropZone], and `desktop_drop` delivers a
  /// drop to every mounted target rather than only the innermost — so without
  /// claiming, dropping on a conversation both sent the file to the peer and
  /// staged it for the Send flow. Claiming makes this zone the only one that
  /// answers while the conversation is open.
  ValueNotifier<int>? _claims;
  bool _claimed = false;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    if (_claims != null) return;
    final claims = DropClaims.maybeOf(context);
    if (claims == null) return;
    _claims = claims;
    // After the frame, never during it: this runs while the screen is being
    // built, and notifying a listener that rebuilds mid-build is an error.
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      _claimed = true;
      claims.value++;
    });
  }

  @override
  void dispose() {
    // Only if the claim was actually taken — a screen disposed inside the same
    // frame it mounted never reached the callback above, and decrementing then
    // would hand the outer zone a negative count it could never clear.
    if (_claimed) _claims!.value--;
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
