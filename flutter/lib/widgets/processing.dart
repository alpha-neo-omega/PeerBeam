import 'dart:async';

import 'package:flutter/material.dart';

import '../app/theme.dart';

/// Run [action] while showing a blocking spinner dialog ([message]), dismissed
/// when it completes. For slow operations with no other visible feedback — e.g.
/// on Android a large picked file is streamed into app storage before it can be
/// sent, which is otherwise invisible and looks frozen.
///
/// Safe if the widget is unmounted before [action] finishes (the dialog is
/// popped via its own captured context).
Future<T> withProcessing<T>(
  BuildContext context,
  String message,
  Future<T> Function() action,
) async {
  BuildContext? dialogContext;
  // Fire-and-forget: shows the dialog; the future resolves when it's popped.
  unawaited(
    showDialog<void>(
      context: context,
      barrierDismissible: false,
      useRootNavigator: true,
      builder: (ctx) {
        dialogContext = ctx;
        return PopScope(
          canPop: false,
          child: AlertDialog(
            // Column with a centered spinner above centered text — unambiguously
            // centered regardless of dialog width.
            content: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                const SizedBox(
                  width: 30,
                  height: 30,
                  child: CircularProgressIndicator(strokeWidth: 3),
                ),
                const Gap(AppSpace.md),
                Text(message, textAlign: TextAlign.center),
              ],
            ),
          ),
        );
      },
    ),
  );
  try {
    return await action();
  } finally {
    final ctx = dialogContext;
    if (ctx != null && ctx.mounted) Navigator.of(ctx).pop();
  }
}
