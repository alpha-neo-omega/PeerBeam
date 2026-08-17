import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:open_filex/open_filex.dart';

/// Whether [path] names a file whose bytes are on this device **right now**.
///
/// A chat row's `localPath` records where its file was, not where it still is:
/// the sender's original can be moved, renamed or deleted after the row is
/// written, and on Android a received file's engine-private copy is unlinked
/// once it has been published into the user's SAF folder, so that path dangles
/// by design. Anything that intends to hand the path to the engine — forwarding
/// a message, for one — has to ask first and say so when the answer is no,
/// rather than letting the send fail one file at a time.
///
/// Deliberately synchronous and deliberately here, beside [openLocalPath],
/// which asks the same question for the same reason.
bool localFileExists(String path) =>
    path.isNotEmpty && FileSystemEntity.isFileSync(path);

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
