import 'package:flutter/material.dart';

import '../app/theme.dart';

/// Subtle entrance animation: fade + rise, staggered by [index] so lists
/// cascade in. Uses implicit animations only (nothing to dispose).
class Appear extends StatefulWidget {
  final int index;
  final Widget child;
  const Appear({super.key, this.index = 0, required this.child});

  @override
  State<Appear> createState() => _AppearState();
}

class _AppearState extends State<Appear> {
  bool _visible = false;

  bool _scheduled = false;

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    if (_scheduled) return;
    _scheduled = true;
    // Reduced motion: appear immediately, no stagger.
    if (!AppMotion.enabled(context)) {
      _visible = true;
      return;
    }
    final delay = Duration(milliseconds: (widget.index * 45).clamp(0, 320));
    Future.delayed(delay, () {
      if (mounted) setState(() => _visible = true);
    });
  }

  @override
  Widget build(BuildContext context) {
    final d = AppMotion.duration(context, AppMotion.medium);
    return AnimatedSlide(
      offset: _visible ? Offset.zero : const Offset(0, 0.06),
      duration: d,
      curve: AppMotion.curve,
      child: AnimatedOpacity(
        opacity: _visible ? 1 : 0,
        duration: d,
        curve: AppMotion.curve,
        child: widget.child,
      ),
    );
  }
}
