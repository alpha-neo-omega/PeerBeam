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

/// A spinner the user can get out of.
///
/// [withProcessing]'s barrier is deliberate — a file being streamed into app
/// storage must not be interrupted halfway — but it is wrong for an operation
/// whose whole duration is someone else's to decide. Asking an address who is
/// there dials it: `CONNECT_TIMEOUT` is 8s **per resolved address**, and a
/// MagicDNS name resolves to several, and a peer that answers and then stalls
/// mid-handshake holds the call for the 120s `AUTH_TIMEOUT`. Two minutes
/// behind a `canPop: false` barrier is not a slow operation, it is a hostage
/// situation.
///
/// Returns null when the user gave up.
///
/// **Honest about what cancelling does.** It stops *waiting*; it does not
/// reach into the engine and abort the dial, which runs to its own conclusion
/// and tidies up after itself. Nothing is left half-done by leaving — the
/// engine's own timeouts bound it — and whatever it learns is still recorded,
/// so a later attempt may well be instant. The copy says "Stop waiting" rather
/// than "Cancel" for exactly that reason: "Cancel" would promise an abort this
/// cannot perform.
Future<T?> withCancellableProcessing<T>(
  BuildContext context,
  String message,
  Future<T> Function() action,
) async {
  final gaveUp = Completer<void>();
  BuildContext? dialogContext;
  ModalRoute<void>? dialogRoute;

  unawaited(
    showDialog<void>(
      context: context,
      barrierDismissible: false,
      useRootNavigator: true,
      builder: (ctx) {
        dialogContext = ctx;
        dialogRoute = ModalRoute.of(ctx);
        return PopScope(
          // Back counts as giving up, rather than being swallowed. A back
          // press that did nothing is how a dialog teaches someone the app has
          // frozen.
          canPop: false,
          onPopInvokedWithResult: (didPop, _) {
            if (!gaveUp.isCompleted) gaveUp.complete();
          },
          child: AlertDialog(
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
            actions: [
              TextButton(
                onPressed: () {
                  if (!gaveUp.isCompleted) gaveUp.complete();
                },
                child: const Text('Stop waiting'),
              ),
            ],
          ),
        );
      },
    ),
  );

  void dismiss() {
    final ctx = dialogContext;
    final route = dialogRoute;
    if (ctx == null || !ctx.mounted) return;
    if (route != null && route.isActive) {
      Navigator.of(ctx).removeRoute(route);
      return;
    }
    Navigator.of(ctx).pop();
  }

  try {
    // `action()` is started once and raced. The losing side is not cancelled —
    // see the note above — only stopped being waited on.
    final result = await Future.any<Object?>([
      action(),
      gaveUp.future.then((_) => _gaveUp),
    ]);
    return identical(result, _gaveUp) ? null : result as T;
  } finally {
    if (dialogContext != null) {
      dismiss();
    } else {
      // Same frame race as [withProcessing]; see `dismiss` there for why this
      // removes its own route rather than popping the top one.
      WidgetsBinding.instance.addPostFrameCallback((_) => dismiss());
    }
  }
}

/// A sentinel for "the user stopped waiting", distinct from any value [T] a
/// caller's action could legitimately return — including null.
final Object _gaveUp = Object();
