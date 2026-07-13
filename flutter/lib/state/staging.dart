import 'package:flutter/foundation.dart';

/// A file queued to be sent (from a drag & drop or picker). Only path +
/// metadata are held — never the file's bytes — so staging a 50 GB file costs
/// nothing.
@immutable
class StagedFile {
  final String path;
  final String name;
  final int size;
  final bool isDirectory;

  const StagedFile({
    required this.path,
    required this.name,
    required this.size,
    this.isDirectory = false,
  });
}

/// Holds the set of files staged for sending. Pure and UI-agnostic: the drop
/// zone feeds it, screens render it. Deduplicates by path so dropping the same
/// file twice is a no-op.
class StagingStore extends ChangeNotifier {
  final List<StagedFile> _items = [];

  List<StagedFile> get items => List.unmodifiable(_items);
  int get count => _items.length;
  bool get isEmpty => _items.isEmpty;
  int get totalBytes => _items.fold(0, (sum, f) => sum + f.size);

  /// Add files, ignoring any whose path is already staged. Returns how many
  /// were newly added.
  int add(Iterable<StagedFile> files) {
    var added = 0;
    for (final f in files) {
      if (_items.any((e) => e.path == f.path)) continue;
      _items.add(f);
      added++;
    }
    if (added > 0) notifyListeners();
    return added;
  }

  void remove(String path) {
    final before = _items.length;
    _items.removeWhere((f) => f.path == path);
    if (_items.length != before) notifyListeners();
  }

  void clear() {
    if (_items.isEmpty) return;
    _items.clear();
    notifyListeners();
  }
}
