import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../sdk/error_text.dart';
import '../../state/app_scope.dart';
import '../../state/models.dart';
import '../../state/staging.dart';
import '../../widgets/appear.dart';
import 'pick_device.dart';

/// Show the staged-files sheet (opened after a drop). Lists what will be sent,
/// with per-file removal and a total; the actual send wires in with the engine.
Future<void> showStagedFilesSheet(BuildContext context, StagingStore staging) {
  return showModalBottomSheet<void>(
    context: context,
    showDragHandle: true,
    isScrollControlled: true,
    builder: (context) => _StagedSheet(staging: staging),
  );
}

/// Choose a destination (nearby or saved) and send all staged files to it.
Future<void> _send(BuildContext context, StagingStore staging) async {
  final scope = AppScope.of(context);
  void snack(String m) => ScaffoldMessenger.of(context)
    ..hideCurrentSnackBar()
    ..showSnackBar(SnackBar(content: Text(m)));

  final picked = await showDevicePicker(context);
  if (picked == null || !context.mounted) return;

  final items = staging.items;
  final files = items.where((f) => !f.isDirectory).map((f) => f.path).toList();
  final folders = items.where((f) => f.isDirectory).map((f) => f.path).toList();
  try {
    if (files.isNotEmpty) await scope.transfer.send(picked.target, files);
    for (final folder in folders) {
      await scope.transfer.sendFolder(picked.target, folder);
    }
    staging.clear();
    if (context.mounted) {
      Navigator.pop(context); // close the staged sheet
      snack('Sending ${items.length} to ${picked.name}');
    }
  } catch (e) {
    if (context.mounted) snack(friendlyError(e));
  }
}

class _StagedSheet extends StatelessWidget {
  final StagingStore staging;
  const _StagedSheet({required this.staging});

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;

    return AnimatedBuilder(
      animation: staging,
      builder: (context, _) {
        final items = staging.items;
        return SafeArea(
          child: ConstrainedBox(
            constraints: BoxConstraints(
              maxHeight: MediaQuery.sizeOf(context).height * 0.7,
            ),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                Padding(
                  padding: const EdgeInsets.fromLTRB(
                    AppSpace.lg,
                    AppSpace.xxs,
                    AppSpace.sm,
                    AppSpace.xs,
                  ),
                  child: Row(
                    children: [
                      Text(
                        'Ready to send',
                        style: text.titleLarge?.copyWith(
                          fontWeight: FontWeight.w700,
                        ),
                      ),
                      const Spacer(),
                      if (items.isNotEmpty)
                        TextButton(
                          onPressed: staging.clear,
                          child: const Text('Clear'),
                        ),
                    ],
                  ),
                ),
                if (items.isEmpty)
                  Padding(
                    padding: const EdgeInsets.all(AppSpace.xxl),
                    child: Text(
                      'No files staged.',
                      style: text.bodyMedium?.copyWith(
                        color: scheme.onSurfaceVariant,
                      ),
                    ),
                  )
                else
                  Flexible(
                    child: ListView.builder(
                      shrinkWrap: true,
                      padding: const EdgeInsets.symmetric(
                        horizontal: AppSpace.sm,
                      ),
                      itemCount: items.length,
                      itemBuilder: (context, i) => Appear(
                        index: i,
                        child: ListTile(
                          leading: Icon(
                            items[i].isDirectory
                                ? Icons.folder_rounded
                                : Icons.insert_drive_file_rounded,
                            color: scheme.primary,
                          ),
                          title: Text(
                            items[i].name,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                          ),
                          subtitle: Text(
                            items[i].isDirectory
                                ? 'Folder'
                                : formatBytes(items[i].size),
                          ),
                          trailing: IconButton(
                            icon: const Icon(Icons.close_rounded),
                            tooltip: 'Remove ${items[i].name}',
                            onPressed: () => staging.remove(items[i].path),
                          ),
                        ),
                      ),
                    ),
                  ),
                Padding(
                  padding: const EdgeInsets.all(AppSpace.md),
                  child: Row(
                    children: [
                      Expanded(
                        child: Text(
                          items.isEmpty
                              ? ''
                              : '${items.length} ${items.length == 1 ? 'item' : 'items'} · ${formatBytes(staging.totalBytes)}',
                          style: text.bodyMedium?.copyWith(
                            color: scheme.onSurfaceVariant,
                          ),
                        ),
                      ),
                      FilledButton.icon(
                        onPressed: items.isEmpty
                            ? null
                            : () => _send(context, staging),
                        icon: const Icon(Icons.send_rounded),
                        label: Text('Send ${items.length}'),
                      ),
                    ],
                  ),
                ),
              ],
            ),
          ),
        );
      },
    );
  }
}
