import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:path_provider/path_provider.dart';

/// Build the JSON config passed to `PeerBeamApi.initialize`.
///
/// On desktop this returns `''`: the Rust engine's `dirs`-based defaults resolve
/// the real Downloads and data directories correctly, so no override is needed.
///
/// On Android the `dirs` crate returns no Downloads/data directory and silently
/// falls back to an app-private temp dir — received files land somewhere the
/// user can't see (and the OS may evict), and the trust store / history /
/// settings don't persist across restarts. So there we supply real paths from
/// `path_provider`:
///   - `save_directory`: app-specific external storage
///     (`.../Android/data/<app-id>/files/PeerBeam`, angle brackets literal) —
///     writable with no runtime permission and browsable in a file manager.
///   - `data_directory`: the app support directory — private and persistent, the
///     right home for trust.json / history.json / ffi_settings.json.
///
/// Any failure falls back to `''` (engine defaults) rather than blocking init.
Future<String> buildEngineConfigJson() async {
  if (kIsWeb || !Platform.isAndroid) return '';
  try {
    final support = await getApplicationSupportDirectory();
    final external =
        await getExternalStorageDirectory() ??
        await getApplicationDocumentsDirectory();
    final saveDir = Directory('${external.path}/PeerBeam');
    await saveDir.create(recursive: true);
    return jsonEncode({
      'storage': {
        'save_directory': saveDir.path,
        'data_directory': support.path,
      },
    });
  } catch (_) {
    return '';
  }
}
