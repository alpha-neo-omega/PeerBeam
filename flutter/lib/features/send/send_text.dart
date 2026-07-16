import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../app/theme.dart';
import '../../state/app_scope.dart';
import 'staged_sheet.dart';

/// The wire-name convention for a text/clipboard payload.
final RegExp messageFileName = RegExp(r'^peerbeam-clipboard-\d+\.txt$');

/// Monotonic, digits-only sequence for temp payload names — seeded from the
/// clock so temp files from different runs never collide, and guaranteed unique
/// within a run (two texts in one stack would otherwise share a millisecond).
int _payloadSeq = DateTime.now().microsecondsSinceEpoch;

/// Write [text] to a temp file using the wire convention
/// (`peerbeam-clipboard-<digits>.txt`) and return its path. The receiver and
/// History render files with this name as a message, not a downloaded file.
Future<String> writeTextPayload(String text) async {
  final file = File(
    '${Directory.systemTemp.path}/peerbeam-clipboard-${_payloadSeq++}.txt',
  );
  await file.writeAsString(text);
  return file.path;
}

/// The compose-message dialog. Returns the entered text, or null if cancelled.
Future<String?> composeText(BuildContext context, {String prefill = ''}) async {
  final controller = TextEditingController(text: prefill);
  try {
    return await showDialog<String>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('Send text'),
        content: TextField(
          controller: controller,
          autofocus: true,
          minLines: 3,
          maxLines: 8,
          decoration: const InputDecoration(
            hintText: 'Type or paste a message',
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(ctx, controller.text),
            child: const Text('Add'),
          ),
        ],
      ),
    );
  } finally {
    controller.dispose();
  }
}

/// Compose a message (prefilled from the clipboard) and add it to the stack,
/// then open the selection tray — the LocalSend-style "add text" flow.
Future<void> addTextToStack(BuildContext context) async {
  final clip = (await Clipboard.getData(Clipboard.kTextPlain))?.text ?? '';
  if (!context.mounted) return;
  final text = await composeText(context, prefill: clip);
  if (text == null || text.trim().isEmpty || !context.mounted) return;
  final staging = AppScope.of(context).staging;
  staging.addText(text);
  showStagedFilesSheet(context, staging);
}

/// Add the current clipboard text to the stack (no dialog). Shows a dialog
/// if empty — a SnackBar would render behind the modal staged sheet this is
/// always called from and never be seen.
Future<void> addClipboardToStack(BuildContext context) async {
  final clip = (await Clipboard.getData(Clipboard.kTextPlain))?.text ?? '';
  if (!context.mounted) return;
  if (clip.trim().isEmpty) {
    await showDialog<void>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('Clipboard is empty'),
        content: const Text('There is no text on the clipboard to add.'),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx),
            child: const Text('OK'),
          ),
        ],
      ),
    );
    return;
  }
  AppScope.of(context).staging.addText(clip);
}

/// Show a text payload as a message dialog (content + Copy), like LocalSend —
/// instead of it looking like a downloaded file. [title] e.g. "Message from X".
Future<void> showMessageDialog(
  BuildContext context, {
  required String title,
  required String text,
}) {
  return showDialog<void>(
    context: context,
    builder: (ctx) {
      final scheme = Theme.of(ctx).colorScheme;
      return AlertDialog(
        title: Text(title),
        content: ConstrainedBox(
          constraints: const BoxConstraints(maxHeight: 320),
          child: SingleChildScrollView(
            child: Container(
              width: double.maxFinite,
              padding: const EdgeInsets.all(AppSpace.sm),
              decoration: BoxDecoration(
                color: scheme.surfaceContainerHighest,
                borderRadius: BorderRadius.circular(AppRadius.md),
              ),
              child: SelectableText(text),
            ),
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx),
            child: const Text('Close'),
          ),
          FilledButton.icon(
            onPressed: () {
              Clipboard.setData(ClipboardData(text: text));
              Navigator.pop(ctx);
              ScaffoldMessenger.of(ctx)
                ..hideCurrentSnackBar()
                ..showSnackBar(const SnackBar(content: Text('Copied')));
            },
            icon: const Icon(Icons.copy_rounded, size: AppIcons.sm),
            label: const Text('Copy'),
          ),
        ],
      );
    },
  );
}
