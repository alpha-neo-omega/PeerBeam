import 'package:flutter/material.dart';

import '../app/theme.dart';
import '../sdk/error_text.dart';

/// Caps content width on large panes for readable line length and centres it.
class ContentPane extends StatelessWidget {
  final Widget child;
  final double maxWidth;
  const ContentPane({
    super.key,
    required this.child,
    this.maxWidth = Breakpoints.contentMaxWidth,
  });

  @override
  Widget build(BuildContext context) {
    return Align(
      alignment: Alignment.topCenter,
      child: ConstrainedBox(
        constraints: BoxConstraints(maxWidth: maxWidth),
        child: child,
      ),
    );
  }
}

/// A titled section header with optional trailing action.
class SectionHeader extends StatelessWidget {
  final String title;
  final Widget? trailing;
  const SectionHeader({super.key, required this.title, this.trailing});

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(
        AppSpace.xxs,
        AppSpace.xs,
        AppSpace.xxs,
        AppSpace.xs,
      ),
      child: Row(
        children: [
          Expanded(
            child: Text(title, style: Theme.of(context).textTheme.titleMedium),
          ),
          ?trailing,
        ],
      ),
    );
  }
}

/// A quiet empty state: icon, one-line title, short hint.
class EmptyState extends StatelessWidget {
  final IconData icon;
  final String title;
  final String message;
  final Widget? action;
  const EmptyState({
    super.key,
    required this.icon,
    required this.title,
    required this.message,
    this.action,
  });

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final text = Theme.of(context).textTheme;
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(AppSpace.xxl),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            CircleAvatar(
              radius: 32,
              backgroundColor: scheme.surfaceContainerHighest,
              child: Icon(
                icon,
                size: AppIcons.lg,
                color: scheme.onSurfaceVariant,
              ),
            ),
            const Gap(AppSpace.md),
            Text(title, textAlign: TextAlign.center, style: text.titleMedium),
            const Gap(AppSpace.xxs),
            Text(
              message,
              textAlign: TextAlign.center,
              style: text.bodyMedium?.copyWith(color: scheme.onSurfaceVariant),
            ),
            if (action != null) ...[const Gap(AppSpace.lg), action!],
          ],
        ),
      ),
    );
  }
}

/// What a screen shows when the read it needed did not come back.
///
/// # Why this exists as its own thing
///
/// Five screens loaded their data in an unguarded `_load()`: the throw escaped
/// the post-frame callback, the `_loading` flag never flipped back, and the
/// body stayed an empty `SizedBox` — permanently. Browsing a device that has
/// gone to sleep is the *normal* case for a peer-to-peer app on a LAN, and it
/// produced a white page with a back arrow and no explanation. From the user's
/// side that is indistinguishable from a crash.
///
/// An absence and a failure must never render the same. [`EmptyState`] says
/// "there is nothing here", which is a fact about the world; this says "we
/// could not find out", which is a fact about us — and unlike the first, it
/// comes with something to do about it.
class ErrorState extends StatelessWidget {
  /// The failure, rendered through [friendlyError] so the user sees a sentence
  /// rather than an exception.
  final Object error;

  /// Runs the read again. Required: an error the user can only stare at is
  /// barely better than the blank page this replaced.
  final VoidCallback onRetry;

  /// What failed, in the user's terms — "Could not open that folder", not
  /// "Error". Defaults to something honest but vague for callers with nothing
  /// better to say.
  final String title;

  const ErrorState({
    super.key,
    required this.error,
    required this.onRetry,
    this.title = 'That did not load',
  });

  @override
  Widget build(BuildContext context) => EmptyState(
    icon: Icons.error_outline_rounded,
    title: title,
    message: friendlyError(error),
    action: FilledButton.tonalIcon(
      onPressed: onRetry,
      icon: const Icon(Icons.refresh_rounded),
      label: const Text('Try again'),
    ),
  );
}
