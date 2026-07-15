import 'dart:convert';

import 'package:flutter/foundation.dart';

/// What kind of thing is staged. Files/folders carry a real [path]; text
/// carries inline [text] (materialized to a temp file only at send time).
enum StagedKind { file, folder, text }

/// An item queued to be sent. For files/folders only path + metadata are held
/// (never bytes), so staging a 50 GB file costs nothing. For text the (small)
/// content is held inline until send.
@immutable
class StagedFile {
  /// Stable identity + removal key. For files/folders this is the [path]; for
  /// text it is a store-supplied counter id.
  final String id;
  final String path;
  final String name;
  final int size;
  final StagedKind kind;

  /// Inline content for text items; null for files/folders.
  final String? text;

  const StagedFile._({
    required this.id,
    required this.path,
    required this.name,
    required this.size,
    required this.kind,
    this.text,
  });

  /// A staged file or folder. Identity is the [path] (dedup key).
  factory StagedFile({
    required String path,
    required String name,
    required int size,
    bool isDirectory = false,
  }) => StagedFile._(
    id: path,
    path: path,
    name: name,
    size: size,
    kind: isDirectory ? StagedKind.folder : StagedKind.file,
  );

  /// A staged text message. [id] must be unique per item (the store supplies a
  /// counter-based id). Size is the UTF-8 byte length of [content].
  factory StagedFile.text({required String id, required String content}) =>
      StagedFile._(
        id: id,
        path: '',
        name: 'Text message',
        size: utf8.encode(content).length,
        kind: StagedKind.text,
        text: content,
      );

  bool get isDirectory => kind == StagedKind.folder;
  bool get isText => kind == StagedKind.text;

  /// A short single-line preview for text items ('' for files/folders).
  String get preview {
    final t = text;
    if (t == null) return '';
    final firstLine = t.trimLeft().split('\n').first;
    return firstLine.length > 80 ? '${firstLine.substring(0, 80)}…' : firstLine;
  }
}

/// Holds the set of items staged for sending. Pure and UI-agnostic: pickers /
/// drop zone / composer feed it, screens render it. Files/folders deduplicate
/// by path; text items are always distinct.
class StagingStore extends ChangeNotifier {
  final List<StagedFile> _items = [];
  int _textSeq = 0;

  List<StagedFile> get items => List.unmodifiable(_items);
  int get count => _items.length;
  bool get isEmpty => _items.isEmpty;
  bool get isNotEmpty => _items.isNotEmpty;
  int get totalBytes => _items.fold(0, (sum, f) => sum + f.size);

  /// Add files/folders, ignoring any whose path is already staged. Returns how
  /// many were newly added.
  int add(Iterable<StagedFile> files) {
    var added = 0;
    for (final f in files) {
      if (f.path.isNotEmpty && _items.any((e) => e.path == f.path)) continue;
      _items.add(f);
      added++;
    }
    if (added > 0) notifyListeners();
    return added;
  }

  /// Add a text message and return the created item (its [StagedFile.id] is the
  /// removal key).
  StagedFile addText(String content) {
    final item = StagedFile.text(id: 'text-${_textSeq++}', content: content);
    _items.add(item);
    notifyListeners();
    return item;
  }

  void remove(String id) {
    final before = _items.length;
    _items.removeWhere((f) => f.id == id);
    if (_items.length != before) notifyListeners();
  }

  void clear() {
    if (_items.isEmpty) return;
    _items.clear();
    notifyListeners();
  }
}
