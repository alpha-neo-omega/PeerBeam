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

/// What kind of file the attach menu is asking for.
///
/// This is the platform layer's own vocabulary: the UI names *what the user
/// wants* (a menu choice), and only this file knows how that turns into an
/// actual OS-level filter — a `file_selector` `XTypeGroup` on desktop, an
/// `EXTRA_MIME_TYPES` list on Android. Neither caller passes a raw MIME
/// string down; that mapping lives in exactly one place, below.
enum AttachKind {
  /// Any file, no filter — today's behaviour, kept for the "Document" choice.
  any,

  /// Images and video, for the "Photos & videos" choice.
  media,

  /// Audio only.
  audio,
}

/// Extensions `file_selector` shows for [AttachKind.media]/[AttachKind.audio]
/// on desktop, alongside the MIME types below.
///
/// **Both lists are required**, because the three desktop backends do not
/// agree on which one they read:
///
///  * **Windows** — `file_selector_windows` builds its filter spec from
///    `extensions` alone and never looks at `mimeTypes`, so a MIME-only group
///    offers nothing at all;
///  * **macOS** — `file_selector_macos` maps each MIME through
///    `UTType(mimeType:)` and `compactMap`s the nils away, and
///    `UTType(mimeType: "image/*")` *is* nil, so a wildcard silently drops and
///    only the extensions survive;
///  * **Linux** — `file_selector_linux` adds both
///    `gtk_file_filter_add_pattern` and `gtk_file_filter_add_mime_type`, so it
///    honours `image/*` and is the one backend either list alone would satisfy.
///
/// So the platforms that need the extensions are Windows and macOS, not Linux.
/// Neither list claims to be exhaustive — just the formats people actually
/// send.
const _imageExtensions = [
  'jpg',
  'jpeg',
  'png',
  'gif',
  'webp',
  'bmp',
  'heic',
  'heif',
  'svg',
  'tif',
  'tiff',
];
const _videoExtensions = [
  'mp4',
  'mov',
  'mkv',
  'avi',
  'webm',
  'm4v',
  'wmv',
  'flv',
  '3gp',
];
const _audioExtensions = [
  'mp3',
  'wav',
  'flac',
  'aac',
  'ogg',
  'm4a',
  'wma',
  'opus',
  'aiff',
];

extension on AttachKind {
  /// MIME types to pass through the `pickFiles` method channel as
  /// `EXTRA_MIME_TYPES`, or empty for [AttachKind.any] — sent as no argument
  /// at all (see [pickFilesToStage]), which is the wildcard default an older
  /// native side already implements.
  List<String> get androidMimeTypes => switch (this) {
    AttachKind.any => const [],
    AttachKind.media => const ['image/*', 'video/*'],
    AttachKind.audio => const ['audio/*'],
  };

  /// `file_selector` filter groups for desktop, or null for [AttachKind.any]
  /// (no `acceptedTypeGroups` — every file is offered, today's behaviour).
  List<XTypeGroup>? get desktopTypeGroups => switch (this) {
    AttachKind.any => null,
    AttachKind.media => const [
      XTypeGroup(
        label: 'Photos & videos',
        mimeTypes: ['image/*', 'video/*'],
        extensions: [..._imageExtensions, ..._videoExtensions],
      ),
    ],
    AttachKind.audio => const [
      XTypeGroup(
        label: 'Audio',
        mimeTypes: ['audio/*'],
        extensions: _audioExtensions,
      ),
    ],
  };
}

/// Open the native file picker and return the chosen files as staged entries
/// (path + size only — never read into memory). Empty if cancelled.
///
/// [kind] narrows what the picker offers (see [AttachKind]); a file the
/// filter excludes is simply never shown, rather than picked and rejected
/// afterwards. Defaults to [AttachKind.any] — every existing caller that
/// wants today's unfiltered picker needs no change.
///
/// [keep] is the set of paths the caller currently holds staged elsewhere
/// (typically a [StagingStore]'s own paths). On Android the native side
/// streams every pick into its own cache and prunes that cache by age on
/// each new pick; passing the paths still staged is what stops it pruning a
/// batch the app has not finished with yet, however long ago it was picked.
/// Desktop ignores it: file_selector hands back the user's own filesystem
/// path directly, with no intermediate cache copy for anything to prune.
///
/// On Android this goes through a native `ACTION_OPEN_DOCUMENT` picker
/// (`peerbeam/android`'s `pickFiles`) instead of file_selector: the
/// file_selector_android plugin reads the entire picked file into a Java
/// byte[] before returning, which OOMs on large files under this app's
/// 256MB heap cap. The native side streams each pick into app cache and
/// returns paths only. Desktop keeps file_selector, which already hands back
/// a real filesystem path with no byte copy.
Future<List<StagedFile>> pickFilesToStage({
  AttachKind kind = AttachKind.any,
  List<String> keep = const [],
}) async {
  if (!kIsWeb && defaultTargetPlatform == TargetPlatform.android) {
    const channel = MethodChannel('peerbeam/android');
    final mimeTypes = kind.androidMimeTypes;
    // No argument at all when neither is needed, not an empty map — an
    // older native build (or a test stub) that only understands a bare
    // `pickFiles` call must keep behaving exactly as it does today.
    final args = <String, Object?>{
      if (mimeTypes.isNotEmpty) 'mimeTypes': mimeTypes,
      if (keep.isNotEmpty) 'keep': keep,
    };
    final raw =
        await channel.invokeListMethod<Object?>(
          'pickFiles',
          args.isEmpty ? null : args,
        ) ??
        const [];
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
  final groups = kind.desktopTypeGroups;
  final files = await openFiles(acceptedTypeGroups: groups ?? const []);
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
///
/// # Known gap: the path this returns does not survive a relaunch on macOS
///
/// `macos/Runner/{DebugProfile,Release}.entitlements` both set
/// `com.apple.security.app-sandbox`, so choosing a folder outside the app's
/// container grants this **process** a sandbox extension for it — nothing more.
/// A path is all `file_selector_macos` ever hands back (`FileSelectorPlugin`
/// replies with `selection?.path`, never the `NSURL`), so that is all we can
/// persist, and on the next launch the string is still there while the
/// permission is not: the engine's writes into it fail with EPERM while
/// Settings goes on displaying the folder as chosen. Only a folder the user
/// picks is affected — the default save directory is `dirs`-derived from
/// `$HOME`, which the sandbox already redirects into the container.
///
/// This cannot be repaired from Dart. The durable form of that grant is a
/// security-scoped bookmark, and every part of it is out of reach here:
///
///  * creating one needs the `com.apple.security.files.bookmarks.app-scope`
///    entitlement, which neither entitlements file declares;
///  * `URL.bookmarkData(options: .withSecurityScope)` and
///    `startAccessingSecurityScopedResource()` are Foundation APIs with no Dart
///    binding, and must run against the `NSURL` **while the panel's grant is
///    live** — i.e. inside a native picker we would have to own, not after the
///    fact from a path string;
///  * the consumer is the Rust engine writing through `tokio::fs` in this
///    process, so the resolved URL has to be held open across the engine's
///    whole lifetime — a Swift lifecycle in `macos/Runner` plus opaque bookmark
///    bytes in the engine's settings, not a change to this function.
///
/// Kept as a picked-path-only API rather than pretending otherwise, so the next
/// person to reach for a fix starts at the entitlement instead of here.
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
