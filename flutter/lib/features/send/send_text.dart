import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../app/theme.dart';
import '../../sdk/error_text.dart';
import '../../state/app_scope.dart';
import 'pick_device.dart';

/// Compose a text message (prefilled from the clipboard, editable) and send it
/// to a chosen device — the LocalSend-style "send a message" flow.
Future<void> composeAndSendText(BuildContext context) async {
  final clip = (await Clipboard.getData(Clipboard.kTextPlain))?.text ?? '';
  if (!context.mounted) return;
  final controller = TextEditingController(text: clip);
  final text = await showDialog<String>(
    context: context,
    builder: (ctx) => AlertDialog(
      title: const Text('Send text'),
      content: TextField(
        controller: controller,
        autofocus: true,
        minLines: 3,
        maxLines: 8,
        decoration: const InputDecoration(hintText: 'Type or paste a message'),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(ctx),
          child: const Text('Cancel'),
        ),
        FilledButton(
          onPressed: () => Navigator.pop(ctx, controller.text),
          child: const Text('Send'),
        ),
      ],
    ),
  );
  if (text == null || text.trim().isEmpty || !context.mounted) return;
  await sendTextToDevice(context, text);
}

/// Send a piece of text to a chosen device using the text wire convention
/// (a small `peerbeam-clipboard-*.txt`), so the receiver shows it as a message.
/// Used by the compose flow and by shared-text intents.
Future<void> sendTextToDevice(BuildContext context, String text) async {
  final scope = AppScope.of(context);
  void snack(String m) => ScaffoldMessenger.of(context)
    ..hideCurrentSnackBar()
    ..showSnackBar(SnackBar(content: Text(m)));

  if (text.trim().isEmpty) {
    snack('Nothing to send');
    return;
  }
  final picked = await showDevicePicker(context);
  if (picked == null || !context.mounted) return;
  try {
    final file = File(
      '${Directory.systemTemp.path}/peerbeam-clipboard-'
      '${DateTime.now().millisecondsSinceEpoch}.txt',
    );
    await file.writeAsString(text);
    await scope.transfer.send(picked.target, [file.path]);
    if (context.mounted) snack('Sending message to ${picked.name}');
  } catch (e) {
    if (context.mounted) snack(friendlyError(e));
  }
}

/// The wire-name convention for a text/clipboard payload.
final RegExp messageFileName = RegExp(r'^peerbeam-clipboard-\d+\.txt$');

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
