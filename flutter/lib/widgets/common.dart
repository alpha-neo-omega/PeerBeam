import 'package:flutter/material.dart';

import '../app/theme.dart';

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
