import 'package:flutter/material.dart';

import '../../sdk/error_text.dart';
import '../../sdk/models.dart' show PeerTarget;
import '../../state/app_scope.dart';
import '../../state/staging.dart';
import 'send_text.dart';

/// Send the entire staged selection to [target]: files + text (each text
/// materialized to a temp payload) go in one batch; folders stream one at a
/// time. Clears the stack on success; keeps it on error so the user can retry.
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

  final filePaths = <String>[];
  final folderPaths = <String>[];
  final texts = <String>[];
  for (final item in items) {
    switch (item.kind) {
      case StagedKind.folder:
        folderPaths.add(item.path);
      case StagedKind.text:
        texts.add(item.text ?? '');
      case StagedKind.file:
        filePaths.add(item.path);
    }
  }

  try {
    for (final t in texts) {
      filePaths.add(await writeTextPayload(t));
    }
    if (filePaths.isNotEmpty) await scope.transfer.send(target, filePaths);
    for (final folder in folderPaths) {
      await scope.transfer.sendFolder(target, folder);
    }
    staging.clear();
    if (context.mounted) snack('Sending ${items.length} to $targetName');
  } catch (e) {
    if (context.mounted) snack(friendlyError(e));
  }
}
