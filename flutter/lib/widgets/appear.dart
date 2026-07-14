import 'package:flutter/material.dart';

import '../app/theme.dart';

/// Subtle entrance animation: fade + rise, staggered by [index] so lists
/// cascade in. Timer-free: the stagger is an [Interval] inside one implicit
/// animation, so nothing is left pending when the tree is disposed.
class Appear extends StatelessWidget {
  final int index;
  final Widget child;
  const Appear({super.key, this.index = 0, required this.child});

  @override
  Widget build(BuildContext context) {
    // Reduced motion: appear immediately, no stagger.
    if (!AppMotion.enabled(context)) return child;

    final delayMs = (index * 45).clamp(0, 320);
    final totalMs = delayMs + AppMotion.medium.inMilliseconds;
    final curve = Interval(delayMs / totalMs, 1, curve: AppMotion.curve);

    return TweenAnimationBuilder<double>(
      tween: Tween(begin: 0, end: 1),
      duration: Duration(milliseconds: totalMs),
      curve: curve,
      builder: (context, t, child) => Opacity(
        opacity: t,
        child: Transform.translate(
          offset: Offset(0, (1 - t) * 8),
          child: child,
        ),
      ),
      child: child,
    );
  }
}
