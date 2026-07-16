import 'package:flutter/material.dart';

import '../../sdk/error_text.dart';
import '../../sdk/models.dart' show PeerTarget;
import '../../state/app_scope.dart';
import '../../state/staging.dart';
import 'send_text.dart';

/// Send the entire staged selection to [target]: files + text (each text
/// materialized to a temp payload) go in one batch; folders stream one at a
/// time. Removes only the items that were actually enqueued — a mid-batch
/// failure (e.g. a folder throwing) leaves un-sent items staged so a retry
/// can't duplicate what already went out, and any item the user adds while
/// the send is in flight survives.
Future<void> sendStaged(
  BuildContext context,
  PeerTarget target,
  String targetName,
) async {
  final scope = AppScope.of(context);
  final staging = scope.staging;
  final items = staging.items;
  if (items.isEmpty) return;

  void snack(String m) => ScaffoldMessenger.of(context)
    ..hideCurrentSnackBar()
    ..showSnackBar(SnackBar(content: Text(m)));

  final fileItems = <StagedFile>[];
  final folderItems = <StagedFile>[];
  final textItems = <StagedFile>[];
  for (final item in items) {
    switch (item.kind) {
      case StagedKind.folder:
        folderItems.add(item);
      case StagedKind.text:
        textItems.add(item);
      case StagedKind.file:
        fileItems.add(item);
    }
  }

  final sentIds = <String>[];
  try {
    final batchPaths = [for (final f in fileItems) f.path];
    for (final t in textItems) {
      batchPaths.add(await writeTextPayload(t.text ?? ''));
    }
    if (batchPaths.isNotEmpty) {
      await scope.transfer.send(target, batchPaths);
      sentIds
        ..addAll(fileItems.map((f) => f.id))
        ..addAll(textItems.map((f) => f.id));
    }
    for (final folder in folderItems) {
      await scope.transfer.sendFolder(target, folder.path);
      sentIds.add(folder.id);
    }
  } catch (e) {
    if (context.mounted) snack(friendlyError(e));
  } finally {
    for (final id in sentIds) {
      staging.remove(id);
    }
    if (sentIds.isNotEmpty && context.mounted) {
      snack('Sending ${sentIds.length} to $targetName');
    }
  }
}
