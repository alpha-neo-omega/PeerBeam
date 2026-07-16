import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

/// A destination folder the user granted via the Storage Access Framework.
@immutable
class SafFolder {
  /// The persisted `content://` tree URI (opaque; kept for reference).
  final String uri;

  /// A human-readable folder name for display.
  final String name;

  /// True when this is the zero-config default (public Downloads/PeerBeam), not
  /// a folder the user explicitly picked.
  final bool isDefault;

  const SafFolder({
    required this.uri,
    required this.name,
    this.isDefault = false,
  });
}

/// Storage Access Framework bridge (Android only).
///
/// The Rust engine writes received files via `std::fs` into app storage, which
/// modern Android hides from the Files app and Gallery. SAF lets the user pick a
/// real, visible destination folder once; we persist the grant natively and copy
/// each received file into it. Every call is a safe no-op off Android.
class Saf {
  static const MethodChannel _ch = MethodChannel('peerbeam/android');

  static bool get isSupported =>
      !kIsWeb && defaultTargetPlatform == TargetPlatform.android;

  static SafFolder? _folder(Map<Object?, Object?>? m) => m == null
      ? null
      : SafFolder(
          name: m['name'] as String? ?? 'Folder',
          uri: m['uri'] as String? ?? '',
          isDefault: m['isDefault'] as bool? ?? false,
        );

  /// The currently-chosen destination folder, or null if none is set (or the
  /// grant was revoked).
  static Future<SafFolder?> currentFolder() async {
    if (!isSupported) return null;
    try {
      return _folder(
        await _ch.invokeMethod<Map<Object?, Object?>>('safCurrentFolder'),
      );
    } catch (_) {
      return null;
    }
  }

  /// Launch the system folder picker; returns the chosen folder, or null if the
  /// user cancelled.
  static Future<SafFolder?> pickFolder() async {
    if (!isSupported) return null;
    try {
      return _folder(
        await _ch.invokeMethod<Map<Object?, Object?>>('safPickFolder'),
      );
    } catch (_) {
      return null;
    }
  }

  /// Copy the file at [path] into the chosen folder as [name] (overwriting a
  /// same-name file). Returns true on success, false if no folder is set or the
  /// copy failed.
  static Future<bool> save(String path, String name) async {
    if (!isSupported) return false;
    try {
      final uri = await _ch.invokeMethod<String>('safSave', {
        'path': path,
        'name': name,
      });
      return uri != null;
    } catch (_) {
      return false;
    }
  }

  /// Recursively copy every file under local folder [path] into the chosen
  /// destination (SAF tree if set, else public Downloads/PeerBeam),
  /// preserving the folder's own name and its subdirectory structure. Returns
  /// true only if every file was published.
  static Future<bool> saveTree(String path) async {
    if (!isSupported) return false;
    try {
      return (await _ch.invokeMethod<bool>('safSaveTree', {'path': path})) ??
          false;
    } catch (_) {
      return false;
    }
  }

  /// Open a previously-saved file from the chosen folder by [name]. Returns true
  /// if it was found and an opener was launched.
  static Future<bool> open(String name) async {
    if (!isSupported) return false;
    try {
      return (await _ch.invokeMethod<bool>('safOpen', {'name': name})) ?? false;
    } catch (_) {
      return false;
    }
  }
}
