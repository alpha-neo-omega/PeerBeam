import 'package:flutter/material.dart';

import '../../app/theme.dart';
import '../../platform/desktop_files.dart';
import '../../state/models.dart' show formatBytes;
import '../../state/staging.dart';
import '../../widgets/appear.dart';
import '../../widgets/processing.dart';
import 'pick_device.dart';
import 'send_staged.dart';
import 'send_text.dart';

/// Show the selection tray. Lists what will be sent, with a source toolbar to
/// keep stacking (files/folder/text/clipboard), per-item removal, a total, and
/// a Send action that picks a device and sends the whole stack.
Future<void> showStagedFilesSheet(BuildContext context, StagingStore staging) {
  return showModalBottomSheet<void>(
    context: context,
    showDragHandle: true,
    isScrollControlled: true,
    builder: (context) => _StagedSheet(staging: staging),
  );
}

/// Pick a destination and send the whole stack, then close the sheet.
Future<void> _pickAndSend(BuildContext context, StagingStore staging) async {
  final picked = await showDevicePicker(context);
  if (picked == null || !context.mounted) return;
  await sendStaged(context, picked.target, picked.name);
  if (context.mounted) Navigator.pop(context);
}

Future<void> _addFiles(BuildContext context, StagingStore staging) async {
  final picked = await withProcessing(
    context,
    'Preparing files…',
    () => pickFilesToStage(keep: staging.paths),
  );
  if (picked.isNotEmpty) staging.add(picked);
}

Future<void> _addFolder(BuildContext context, StagingStore staging) async {
  final folder = await pickFolderToStage();
  if (folder != null) staging.add([folder]);
}

Future<void> _addText(BuildContext context, StagingStore staging) async {
  final text = await composeText(context);
  if (text != null && text.trim().isNotEmpty) staging.addText(text);
}

/// Confirm before emptying the tray.
///
/// A prompt rather than an undo, for two reasons. A staged text message exists
/// nowhere but this store — there is no file to pick again — so re-picking
/// cannot rebuild a cleared stack the way it can rebuild a list of files. And
/// the undo that would otherwise belong here cannot be offered: a snackbar
/// raised from inside a modal sheet is drawn beneath it, under the sheet's own
/// barrier, so its action is neither visible nor tappable. Per-item removal
/// stays instant and unprompted, which is what keeps this dialog rare enough to
/// still be read.
Future<void> _confirmClear(BuildContext context, StagingStore staging) async {
  final count = staging.count;
  final confirmed = await showDialog<bool>(
    context: context,
    builder: (ctx) => AlertDialog(
      title: Text('Clear $count ${count == 1 ? 'item' : 'items'}?'),
      content: const Text(
        'Files and folders can be added again. Any text you typed is only '
        'here, and cannot.',
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(ctx, false),
          child: const Text('Cancel'),
        ),
        FilledButton(
          onPressed: () => Navigator.pop(ctx, true),
          child: const Text('Clear'),
        ),
      ],
    ),
  );
  if (confirmed == true) staging.clear();
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
                          onPressed: () => _confirmClear(context, staging),
                          child: const Text('Clear'),
                        ),
                    ],
                  ),
                ),

                // Source toolbar — keep stacking heterogeneous items.
                Padding(
                  padding: const EdgeInsets.symmetric(horizontal: AppSpace.md),
                  child: Wrap(
                    spacing: AppSpace.xs,
                    runSpacing: AppSpace.xs,
                    children: [
                      _SourceButton(
                        icon: Icons.insert_drive_file_rounded,
                        label: 'Files',
                        onTap: () => _addFiles(context, staging),
                      ),
                      if (isDesktop)
                        _SourceButton(
                          icon: Icons.folder_rounded,
                          label: 'Folder',
                          onTap: () => _addFolder(context, staging),
                        ),
                      _SourceButton(
                        icon: Icons.chat_bubble_outline_rounded,
                        label: 'Text',
                        onTap: () => _addText(context, staging),
                      ),
                      _SourceButton(
                        icon: Icons.content_paste_rounded,
                        label: 'Clipboard',
                        onTap: () => addClipboardToStack(context),
                      ),
                    ],
                  ),
                ),
                const Gap(AppSpace.xs),

                if (items.isEmpty)
                  Padding(
                    padding: const EdgeInsets.all(AppSpace.xxl),
                    child: Text(
                      'Add files, a folder, or text to send.',
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
                      itemBuilder: (context, i) {
                        final it = items[i];
                        return Appear(
                          index: i,
                          child: ListTile(
                            leading: Icon(
                              it.isText
                                  ? Icons.chat_bubble_outline_rounded
                                  : it.isDirectory
                                  ? Icons.folder_rounded
                                  : Icons.insert_drive_file_rounded,
                              color: scheme.primary,
                            ),
                            title: Text(
                              it.isText ? 'Text message' : it.name,
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                            ),
                            subtitle: Text(
                              it.isText
                                  ? it.preview
                                  : it.isDirectory
                                  ? 'Folder'
                                  : formatBytes(it.size),
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                            ),
                            trailing: IconButton(
                              icon: const Icon(Icons.close_rounded),
                              tooltip: 'Remove',
                              onPressed: () => staging.remove(it.id),
                            ),
                          ),
                        );
                      },
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
                            : () => _pickAndSend(context, staging),
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

/// A compact "add source" chip-style button for the tray toolbar.
class _SourceButton extends StatelessWidget {
  final IconData icon;
  final String label;
  final VoidCallback onTap;
  const _SourceButton({
    required this.icon,
    required this.label,
    required this.onTap,
  });

  @override
  Widget build(BuildContext context) {
    return FilledButton.tonalIcon(
      onPressed: onTap,
      icon: Icon(icon, size: AppIcons.sm),
      label: Text(label),
    );
  }
}
