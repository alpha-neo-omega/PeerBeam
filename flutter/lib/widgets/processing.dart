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
  // The spinner's OWN route, so it can be taken down by identity rather than
  // by position. See [_dismiss].
  ModalRoute<void>? dialogRoute;
  // Fire-and-forget: shows the dialog; the future resolves when it's popped.
  unawaited(
    showDialog<void>(
      context: context,
      barrierDismissible: false,
      useRootNavigator: true,
      builder: (ctx) {
        dialogContext = ctx;
        dialogRoute = ModalRoute.of(ctx);
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

  /// Take down **this** spinner, wherever it now sits in the stack.
  ///
  /// `Navigator.pop()` was wrong here, and wrong in a way that only showed
  /// when a caller opened a dialog of its own the moment this returned. `pop`
  /// removes the route on **top**, not the one the context belongs to, and the
  /// two are not the same route in the ordinary fast case:
  ///
  ///  1. `action()` completes inside the frame the spinner was requested in,
  ///     so the builder below has not run yet and `dialogContext` is null.
  ///  2. The `finally` therefore books a post-frame `dismiss()` — the retry the
  ///     note there describes.
  ///  3. This function returns, and the caller synchronously opens its own
  ///     dialog, which is pushed on top.
  ///  4. The frame runs. The booked `dismiss()` fires and pops the top — the
  ///     **caller's** dialog — leaving the spinner underneath it, up for good.
  ///
  /// The result was an app wedged behind an undismissable spinner, which is
  /// precisely the failure the post-frame retry was added to prevent. Removing
  /// the route by identity cannot take the wrong one, and `removeRoute` works
  /// whether or not this route is currently on top.
  void dismiss() {
    final ctx = dialogContext;
    final route = dialogRoute;
    if (ctx == null || !ctx.mounted) return;
    if (route != null && route.isActive) {
      Navigator.of(ctx).removeRoute(route);
      return;
    }
    // No route to name — the only case left is a context that is mounted
    // without one, which should not happen; popping is the old behaviour and
    // still better than leaving a barrier up.
    Navigator.of(ctx).pop();
  }

  try {
    return await action();
  } finally {
    if (dialogContext != null) {
      dismiss();
    } else {
      // The route is pushed now but its `builder` — which is what assigns
      // `dialogContext` — does not run until the next frame. An action that
      // completes inside this same frame (a cancelled pick, a mocked channel,
      // anything already resolved) therefore reaches here with nothing to pop,
      // and the barrier of a `PopScope(canPop: false)` dialog would stay up
      // for good, wedging the app behind a spinner. Retry once the frame that
      // builds it has run.
      WidgetsBinding.instance.addPostFrameCallback((_) => dismiss());
    }
  }
}
