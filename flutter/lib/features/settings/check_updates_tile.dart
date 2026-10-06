import 'dart:async';

import 'package:file_selector/file_selector.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';

import '../../platform/open_path.dart';
import '../../sdk/error_text.dart';
import '../../sdk/events.dart';
import '../../sdk/models.dart' show UpdateCheck, UpdateDownload;
import '../../sdk/peerbeam.dart' show PeerBeamApi;
import '../../state/models.dart' show formatBytes;

/// "Check for updates", and the answer — and, when a newer release exists,
/// the way to fetch it.
///
/// Stateful for one reason: the request must not start until someone presses
/// the button, so the result cannot come from anything the screen builds
/// eagerly. Failure is reported as plainly as success — an app that cannot
/// reach the internet is working normally, and amendment A1 makes "never a
/// precondition" a term of the check being allowed to exist.
///
/// # The download, and the terms it runs on
///
/// Amendment A3 permits fetching a release only on terms this tile keeps:
///
/// - **One human action, one download** (condition 1). Finding a newer
///   release offers **Download**; it never starts one.
/// - **"Written to a location that person chose."** Download opens a folder
///   picker, starting at Downloads for convenience, and nothing is fetched
///   unless a folder is confirmed.
/// - **PeerBeam never installs or opens what it downloaded** (condition 4).
///   *Show folder* opens the folder — never the file, which the platform's
///   opener would hand to an installer or a disk-image mounter.
/// - **Quiet when it fails** (condition 5): the outcome is a line of text in
///   this tile, not a dialog, and nothing comes back uninvited.
///
/// Desktop only. Android updates through its own install and has no artifact
/// to fetch here, so it is offered none.
class CheckUpdatesTile extends StatefulWidget {
  final PeerBeamApi? api;

  /// Opens a folder in the platform's file manager.
  ///
  /// Only ever handed the folder the person chose for the download — never
  /// the downloaded file. The default opener passes anything it is given to
  /// the platform's default handler, and for a `.deb`, `.rpm` or `.dmg` that
  /// handler installs or mounts it (A3 condition 4).
  final Future<String?> Function(String folder) openFolder;

  const CheckUpdatesTile({
    super.key,
    required this.api,
    this.openFolder = openLocalPath,
  });

  @override
  State<CheckUpdatesTile> createState() => _CheckUpdatesTileState();
}

class _CheckUpdatesTileState extends State<CheckUpdatesTile> {
  bool _busy = false;
  UpdateCheck? _result;
  Object? _error;

  // The download, once one has been asked for.
  bool _downloading = false;
  ({int done, int? total})? _progress;
  UpdateDownload? _download;
  Object? _downloadError;

  /// The folder the person chose — the only path [CheckUpdatesTile.openFolder]
  /// is ever given.
  String? _folder;
  StreamSubscription<BridgeEvent>? _progressSub;

  /// The platforms a release artifact exists for.
  static const _desktop = {
    TargetPlatform.linux,
    TargetPlatform.macOS,
    TargetPlatform.windows,
  };

  bool get _offerDownload =>
      widget.api != null &&
      _desktop.contains(defaultTargetPlatform) &&
      (_result?.updateAvailable ?? false) &&
      !(_download?.downloaded ?? false);

  @override
  void dispose() {
    _progressSub?.cancel();
    super.dispose();
  }

  Future<void> _check() async {
    final api = widget.api;
    if (api == null || _busy) return;
    setState(() {
      _busy = true;
      _error = null;
      _download = null;
      _downloadError = null;
    });
    try {
      final r = await api.checkForUpdates();
      if (!mounted) return;
      setState(() {
        _result = r;
        _busy = false;
      });
    } catch (e) {
      if (!mounted) return;
      setState(() {
        _error = e;
        _busy = false;
      });
    }
  }

  Future<void> _fetch() async {
    final api = widget.api;
    if (api == null || _downloading) return;
    // A3: "written to a location that person chose". The picker starts at
    // Downloads; nothing is fetched unless a folder is confirmed.
    final folder = await getDirectoryPath(
      initialDirectory: await _downloadsFolder(),
      confirmButtonText: 'Save here',
    );
    if (!mounted || folder == null) return;
    setState(() {
      _downloading = true;
      _progress = null;
      _download = null;
      _downloadError = null;
      _folder = folder;
    });
    _progressSub = api.events.listen((e) {
      if (e is UpdateDownloadProgress && mounted) {
        setState(() => _progress = (done: e.done, total: e.total));
      }
    });
    try {
      final r = await api.downloadUpdate(folder);
      if (mounted) setState(() => _download = r);
    } catch (e) {
      if (mounted) setState(() => _downloadError = e);
    } finally {
      // Not awaited: nothing below depends on the cancel having finished.
      // (A broadcast subscription's cancel future belongs to the root zone, so
      // awaiting it also stalls under a widget test's fake clock.)
      unawaited(_progressSub?.cancel());
      _progressSub = null;
      if (mounted) setState(() => _downloading = false);
    }
  }

  /// Where the picker opens. A convenience only — the person still chooses —
  /// so a platform that cannot say opens the picker wherever it likes.
  static Future<String?> _downloadsFolder() async {
    try {
      return (await getDownloadsDirectory())?.path;
    } catch (_) {
      return null;
    }
  }

  Future<void> _showFolder() async {
    final folder = _folder;
    if (folder == null) return;
    final err = await widget.openFolder(folder);
    if (err != null && mounted) {
      ScaffoldMessenger.maybeOf(
        context,
      )?.showSnackBar(SnackBar(content: Text(err)));
    }
  }

  String get _subtitle {
    if (_downloading) return _downloadingText;
    final d = _download;
    if (d != null) {
      if (d.downloaded) {
        return "Saved ${d.name} to $_folder, checked against the project's "
            'signature. Install it as you would one from the website.';
      }
      if (d.ok) return 'You have ${d.current}, which is the newest.';
      return 'Not downloaded — ${d.reason ?? 'no reason was given'}. '
          'The download address above has it.';
    }
    final de = _downloadError;
    if (de != null) return 'Could not download — ${friendlyError(de)}';
    if (_busy) return 'Asking…';
    final e = _error;
    if (e != null) return 'Could not check — ${friendlyError(e)}';
    final r = _result;
    if (r == null) {
      return 'PeerBeam does not check on its own. Ask, and it will look once.';
    }
    if (!r.reachable) {
      return 'Could not reach the release list. You have ${r.current}.';
    }
    if (r.updateAvailable) {
      return '${r.latest} is available — you have ${r.current}.';
    }
    return 'You have ${r.current}, which is the newest.';
  }

  String get _downloadingText {
    final latest = _result?.latest;
    final what = latest == null ? 'Downloading' : 'Downloading $latest';
    final p = _progress;
    if (p == null) return '$what…';
    final total = p.total;
    if (total != null && total > 0) {
      return '$what… ${(p.done * 100 ~/ total).clamp(0, 100)}%';
    }
    return '$what… ${formatBytes(p.done)}';
  }

  Widget get _trailing {
    if (_busy || _downloading) {
      final p = _progress;
      final total = p?.total;
      return SizedBox.square(
        dimension: 20,
        child: CircularProgressIndicator(
          strokeWidth: 2,
          value: p != null && total != null && total > 0
              ? (p.done / total).clamp(0.0, 1.0)
              : null,
        ),
      );
    }
    if (_download?.downloaded ?? false) {
      return FilledButton.tonal(
        onPressed: _showFolder,
        child: const Text('Show folder'),
      );
    }
    if (_offerDownload) {
      return FilledButton.tonal(
        onPressed: _fetch,
        child: const Text('Download'),
      );
    }
    return FilledButton.tonal(
      onPressed: widget.api == null ? null : _check,
      child: const Text('Check'),
    );
  }

  @override
  Widget build(BuildContext context) => ListTile(
    leading: const Icon(Icons.system_update_alt_rounded),
    title: const Text('Check for updates'),
    subtitle: Text(_subtitle),
    trailing: _trailing,
  );
}
