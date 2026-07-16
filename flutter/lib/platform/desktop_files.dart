import 'package:file_selector/file_selector.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

import '../state/staging.dart';

/// Whether this build runs on a desktop platform.
bool get isDesktop =>
    !kIsWeb &&
    (defaultTargetPlatform == TargetPlatform.linux ||
        defaultTargetPlatform == TargetPlatform.macOS ||
        defaultTargetPlatform == TargetPlatform.windows);

/// Open the native file picker and return the chosen files as staged entries
/// (path + size only — never read into memory). Empty if cancelled.
///
/// On Android this goes through a native `ACTION_OPEN_DOCUMENT` picker
/// (`peerbeam/android`'s `pickFiles`) instead of file_selector: the
/// file_selector_android plugin reads the entire picked file into a Java
/// byte[] before returning, which OOMs on large files under this app's
/// 256MB heap cap. The native side streams each pick into app cache and
/// returns paths only. Desktop keeps file_selector, which already hands back
/// a real filesystem path with no byte copy.
Future<List<StagedFile>> pickFilesToStage() async {
  if (!kIsWeb && defaultTargetPlatform == TargetPlatform.android) {
    const channel = MethodChannel('peerbeam/android');
    final raw =
        await channel.invokeListMethod<Object?>('pickFiles') ?? const [];
    return raw.map((e) {
      final m = Map<Object?, Object?>.from(e as Map);
      return StagedFile(
        path: m['path'] as String,
        name: (m['name'] as String?) ?? '',
        size: (m['size'] as num?)?.toInt() ?? 0,
      );
    }).toList();
  }

  // Desktop: file_selector already returns a real filesystem path — no byte
  // copy involved, so no OOM risk regardless of file size.
  final files = await openFiles();
  final staged = <StagedFile>[];
  for (final f in files) {
    int size = 0;
    try {
      size = await f.length(); // metadata only
    } catch (_) {}
    staged.add(
      StagedFile(
        path: f.path,
        name: f.name.isNotEmpty ? f.name : _basename(f.path),
        size: size,
      ),
    );
  }
  return staged;
}

/// Open the native directory chooser (used to pick the save location). Returns
/// the selected absolute path, or null if cancelled.
Future<String?> pickSaveDirectory() => getDirectoryPath();

/// Pick a folder to send (desktop). Returns it as a staged directory entry,
/// or null if cancelled.
Future<StagedFile?> pickFolderToStage() async {
  final dir = await getDirectoryPath();
  if (dir == null || dir.isEmpty) return null;
  return StagedFile(
    path: dir,
    name: _basename(dir),
    size: 0,
    isDirectory: true,
  );
}

String _basename(String path) {
  final norm = path.replaceAll('\\', '/');
  final i = norm.lastIndexOf('/');
  return i >= 0 ? norm.substring(i + 1) : norm;
}
