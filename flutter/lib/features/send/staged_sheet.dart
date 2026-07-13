import 'package:flutter/material.dart';

import '../../state/models.dart';
import '../../state/staging.dart';
import '../../widgets/appear.dart';

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
                  padding: const EdgeInsets.fromLTRB(20, 4, 12, 8),
                  child: Row(
                    children: [
                      Text(
                        'Ready to send',
                        style: text.titleLarge
                            ?.copyWith(fontWeight: FontWeight.w700),
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
                    padding: const EdgeInsets.all(32),
                    child: Text(
                      'No files staged.',
                      style: text.bodyMedium
                          ?.copyWith(color: scheme.onSurfaceVariant),
                    ),
                  )
                else
                  Flexible(
                    child: ListView.builder(
                      shrinkWrap: true,
                      padding: const EdgeInsets.symmetric(horizontal: 12),
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
                          subtitle: Text(formatBytes(items[i].size)),
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
                  padding: const EdgeInsets.all(16),
                  child: Row(
                    children: [
                      Expanded(
                        child: Text(
                          items.isEmpty
                              ? ''
                              : '${items.length} item(s) · ${formatBytes(staging.totalBytes)}',
                          style: text.bodyMedium
                              ?.copyWith(color: scheme.onSurfaceVariant),
                        ),
                      ),
                      FilledButton.icon(
                        onPressed: items.isEmpty
                            ? null
                            : () {
                                Navigator.pop(context);
                                ScaffoldMessenger.of(context).showSnackBar(
                                  const SnackBar(
                                    content: Text(
                                      'Sending wires in with the engine bridge',
                                    ),
                                  ),
                                );
                              },
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
