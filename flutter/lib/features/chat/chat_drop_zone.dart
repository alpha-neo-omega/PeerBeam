import 'dart:async';

import 'package:desktop_drop/desktop_drop.dart';
import 'package:flutter/material.dart';

import '../../sdk/models.dart';
import '../../state/app_scope.dart';
import '../send/drop_overlay.dart';
import '../send/drop_zone.dart' show collectDroppedFiles, isDesktop;

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
  final Widget child;
  const ChatDropZone({
    super.key,
    required this.peerId,
    required this.peer,
    required this.child,
  });

  @override
  State<ChatDropZone> createState() => _ChatDropZoneState();
}

class _ChatDropZoneState extends State<ChatDropZone> {
  bool _active = false;

  /// A chat file message carries exactly one file, and the engine's
  /// `prepare_file_send` rejects a directory outright — so a dropped folder
  /// cannot go through this path at all. Every non-folder item is sent, one
  /// message each, exactly the fan-out `_attach` already uses for a
  /// multi-select; every folder is skipped and named together in a single
  /// message, rather than vanishing silently or surfacing as an engine error
  /// later.
  Future<void> _onDone(DropDoneDetails detail) async {
    setState(() => _active = false);
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
