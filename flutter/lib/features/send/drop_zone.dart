import 'dart:io';

import 'package:cross_file/cross_file.dart';
import 'package:desktop_drop/desktop_drop.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import '../../state/staging.dart';
import 'drop_overlay.dart';
import 'staged_sheet.dart';

/// Whether this build runs on a desktop platform (drag & drop is desktop-only).
bool get isDesktop =>
    !kIsWeb &&
    (defaultTargetPlatform == TargetPlatform.linux ||
        defaultTargetPlatform == TargetPlatform.macOS ||
        defaultTargetPlatform == TargetPlatform.windows);

/// Wraps [child] with desktop file drag & drop. On non-desktop platforms it is
/// a transparent passthrough. Dropped files are staged (path + size only —
/// never read into memory, so multi-GB and many-file drops are instant) and
/// the staged-files sheet opens.
class DropZone extends StatefulWidget {
  final StagingStore staging;
  final Widget child;
  const DropZone({super.key, required this.staging, required this.child});

  @override
  State<DropZone> createState() => _DropZoneState();
}

class _DropZoneState extends State<DropZone> {
  bool _active = false;

  Future<void> _onDone(DropDoneDetails detail) async {
    setState(() => _active = false);
    final staged = <StagedFile>[];
    for (final XFile file in detail.files) {
      // Folders drop too — flag them so the send splits file vs folder.
      final isDir = FileSystemEntity.isDirectorySync(file.path);
      int size = 0;
      if (!isDir) {
        try {
          size = await file.length(); // metadata only; no read
        } catch (_) {}
      }
      staged.add(
        StagedFile(
          path: file.path,
          name: file.name.isNotEmpty ? file.name : _basename(file.path),
          size: size,
          isDirectory: isDir,
        ),
      );
    }
    final added = widget.staging.add(staged);
    if (added > 0 && mounted) {
      showStagedFilesSheet(context, widget.staging);
    }
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

String _basename(String path) {
  final norm = path.replaceAll('\\', '/');
  final i = norm.lastIndexOf('/');
  return i >= 0 ? norm.substring(i + 1) : norm;
}
