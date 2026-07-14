import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:open_filex/open_filex.dart';

/// Open a local file or directory with the OS default handler.
///
/// Desktop launches the platform opener directly (no plugin needed); mobile
/// goes through `open_filex`, which wraps the content in a FileProvider so
/// Android's `file://` restrictions don't apply.
///
/// Returns a user-facing error message, or null on success.
Future<String?> openLocalPath(String path) async {
  if (path.isEmpty) return 'No local file recorded for this item.';
  final isDir = FileSystemEntity.isDirectorySync(path);
  if (!isDir && !FileSystemEntity.isFileSync(path)) {
    return "That file isn't there any more.";
  }

  if (!kIsWeb && (Platform.isLinux || Platform.isMacOS || Platform.isWindows)) {
    final opener = Platform.isLinux
        ? 'xdg-open'
        : Platform.isMacOS
        ? 'open'
        : 'explorer';
    try {
      await Process.start(opener, [path], mode: ProcessStartMode.detached);
      return null;
    } catch (e) {
      return "Couldn't open it: $e";
    }
  }

  final result = await OpenFilex.open(path);
  return result.type == ResultType.done ? null : result.message;
}
