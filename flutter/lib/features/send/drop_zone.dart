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

/// Collect a completed desktop drop into staged entries: path + size only
/// (metadata, never a read — so multi-GB and many-file drops are instant),
/// with folders flagged via [StagedFile.isDirectory] rather than filtered out
/// here, since what to *do* with a folder is the caller's decision, not this
/// function's.
///
/// Shared by every drop target in the app — [DropZone] (the Send flow) and
/// the chat screen's own drop target both call this rather than each walking
/// [DropDoneDetails] itself, which is exactly the kind of duplication where
/// one of the two copies quietly drifts (a missed folder flag, a dropped size
/// read) while the other is fixed.
Future<List<StagedFile>> collectDroppedFiles(DropDoneDetails detail) async {
  final staged = <StagedFile>[];
  for (final XFile file in detail.files) {
    // Folders drop too — flag them so the caller can split file vs folder.
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
  return staged;
}

/// The window's drop-claim register, published by [DropZone] to everything
/// below it.
///
/// `desktop_drop` registers every mounted `DropTarget` against the same native
/// window, and a nested target does **not** shadow the one above it — both
/// receive the same drop. Since [DropZone] wraps the entire navigation shell,
/// an inner drop target (the chat screen's) would otherwise mean one drop was
/// answered twice: sent to the peer *and* staged for the Send flow, with two
/// overlays lit at once.
///
/// So a target that wants to own drops for the region it covers claims here,
/// and [DropZone] stands down for as long as the claim is held. A counter
/// rather than a flag: two claimants overlapping for a frame (one screen
/// mounting as another disposes) must not leave the outer zone re-enabled
/// early.
class DropClaims extends InheritedWidget {
  final ValueNotifier<int> claims;
  const DropClaims({super.key, required this.claims, required super.child});

  /// The enclosing register, or null when there is none — a chat screen shown
  /// outside the shell (in a test, or a future secondary window) simply owns
  /// its drops uncontested.
  static ValueNotifier<int>? maybeOf(BuildContext context) =>
      context.dependOnInheritedWidgetOfExactType<DropClaims>()?.claims;

  @override
  bool updateShouldNotify(DropClaims oldWidget) => claims != oldWidget.claims;
}

/// Wraps [child] with desktop file drag & drop. On non-desktop platforms it is
/// a transparent passthrough. Dropped files are staged (path + size only —
/// never read into memory, so multi-GB and many-file drops are instant) and
/// the staged-files sheet opens.
///
/// Stands down entirely — no handler, no overlay — while an inner target holds
/// a claim on [DropClaims].
class DropZone extends StatefulWidget {
  final StagingStore staging;
  final Widget child;
  const DropZone({super.key, required this.staging, required this.child});

  @override
  State<DropZone> createState() => _DropZoneState();
}

class _DropZoneState extends State<DropZone> {
  bool _active = false;
  final ValueNotifier<int> _claims = ValueNotifier<int>(0);

  @override
  void initState() {
    super.initState();
    _claims.addListener(_onClaimsChanged);
  }

  @override
  void dispose() {
    _claims.removeListener(_onClaimsChanged);
    _claims.dispose();
    super.dispose();
  }

  /// A claim lands as a screen mounts and is released as one disposes — both
  /// of which happen mid-frame, where calling `setState` is an error. The
  /// rebuild that flips the target on or off is therefore deferred to after
  /// the current frame.
  void _onClaimsChanged() {
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) setState(() {});
    });
  }

  Future<void> _onDone(DropDoneDetails detail) async {
    setState(() => _active = false);
    // Belt and braces for the single frame between a claim landing and the
    // deferred rebuild flipping `enable`: whoever holds the claim is the only
    // one that answers a drop, so this one does nothing rather than staging a
    // copy of what the chat screen just sent.
    if (_claims.value > 0) return;
    final staged = await collectDroppedFiles(detail);
    final added = widget.staging.add(staged);
    if (added > 0 && mounted) {
      showStagedFilesSheet(context, widget.staging);
    }
  }

  @override
  Widget build(BuildContext context) {
    // The register is published even off-desktop, so an inner target's claim
    // and release stay symmetric on every platform rather than depending on
    // which zone happens to be a passthrough.
    if (!isDesktop) return DropClaims(claims: _claims, child: widget.child);

    final owned = _claims.value > 0;
    return DropClaims(
      claims: _claims,
      child: DropTarget(
        enable: !owned,
        onDragEntered: (_) => setState(() => _active = true),
        onDragExited: (_) => setState(() => _active = false),
        onDragDone: _onDone,
        child: Stack(
          fit: StackFit.expand,
          children: [
            widget.child,
            // Never lit while another target owns drops — two overlays for one
            // drag is the visible half of the same bug.
            Positioned.fill(child: DropOverlay(active: _active && !owned)),
          ],
        ),
      ),
    );
  }
}

String _basename(String path) {
  final norm = path.replaceAll('\\', '/');
  final i = norm.lastIndexOf('/');
  return i >= 0 ? norm.substring(i + 1) : norm;
}
