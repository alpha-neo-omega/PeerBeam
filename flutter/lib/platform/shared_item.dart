import 'package:flutter/foundation.dart';

/// An item handed to the app by the OS via a share ("Send to PeerBeam") or a
/// view/open intent.
enum SharedKind { text, file }

@immutable
class SharedItem {
  final SharedKind kind;
  final String? text;
  final String? path;
  final String? name;

  const SharedItem.text(this.text)
      : kind = SharedKind.text,
        path = null,
        name = null;

  const SharedItem.file(this.path, this.name)
      : kind = SharedKind.file,
        text = null;
}

/// Parse a platform event map into shared items. Pure — no channels — so it is
/// fully unit-testable.
///
/// Expected shapes (from the native side):
/// - `{event: 'share', text: '...'}`
/// - `{event: 'share', paths: ['/a', '/b'], names: ['a','b']}`
/// - `{event: 'view', paths: ['/a'], names: ['a']}`
List<SharedItem> parseSharedEvent(Map<String, dynamic> event) {
  final type = event['event'] as String?;
  if (type != 'share' && type != 'view') return const [];

  final items = <SharedItem>[];

  final text = event['text'] as String?;
  if (text != null && text.trim().isNotEmpty) {
    items.add(SharedItem.text(text));
  }

  final paths = (event['paths'] as List?)?.cast<Object?>() ?? const [];
  final names = (event['names'] as List?)?.cast<Object?>() ?? const [];
  for (var i = 0; i < paths.length; i++) {
    final path = paths[i] as String?;
    if (path == null || path.isEmpty) continue;
    final name = i < names.length ? names[i] as String? : null;
    items.add(SharedItem.file(path, name ?? _basename(path)));
  }

  return items;
}

String _basename(String path) {
  final norm = path.replaceAll('\\', '/');
  final i = norm.lastIndexOf('/');
  return i >= 0 ? norm.substring(i + 1) : norm;
}
