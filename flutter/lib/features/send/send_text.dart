import 'dart:io';

import 'package:flutter/material.dart';

import '../../sdk/error_text.dart';
import '../../state/app_scope.dart';
import 'pick_device.dart';

/// Send a piece of text to a chosen device using the clipboard wire convention
/// (a small `peerbeam-clipboard-*.txt`), so the receiver offers one-tap Copy.
/// Used by the Clipboard quick action and by shared-text intents.
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
    if (context.mounted) snack('Sending clipboard to ${picked.name}');
  } catch (e) {
    if (context.mounted) snack(friendlyError(e));
  }
}
