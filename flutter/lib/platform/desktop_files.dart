import 'package:file_selector/file_selector.dart';
import 'package:flutter/foundation.dart';

import '../state/staging.dart';

/// Whether this build runs on a desktop platform.
bool get isDesktop =>
    !kIsWeb &&
    (defaultTargetPlatform == TargetPlatform.linux ||
        defaultTargetPlatform == TargetPlatform.macOS ||
        defaultTargetPlatform == TargetPlatform.windows);

/// Open the native file picker and return the chosen files as staged entries
/// (path + size only — never read into memory). Empty if cancelled or if the
/// platform has no picker. Works on Windows, macOS, and Linux.
Future<List<StagedFile>> pickFilesToStage() async {
  final files = await openFiles();
  final staged = <StagedFile>[];
  for (final f in files) {
    int size = 0;
    try {
      size = await f.length(); // metadata only
    } catch (_) {}
    staged.add(StagedFile(
      path: f.path,
      name: f.name.isNotEmpty ? f.name : _basename(f.path),
      size: size,
    ));
  }
  return staged;
}

/// Open the native directory chooser (used to pick the save location). Returns
/// the selected absolute path, or null if cancelled.
Future<String?> pickSaveDirectory() => getDirectoryPath();

String _basename(String path) {
  final norm = path.replaceAll('\\', '/');
  final i = norm.lastIndexOf('/');
  return i >= 0 ? norm.substring(i + 1) : norm;
}
