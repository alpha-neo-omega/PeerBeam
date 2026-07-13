import 'package:flutter/material.dart';

import '../../app/theme.dart';

/// The visual shown while files are dragged over the window. Driven purely by
/// [active] so it is trivially testable without a native drag. Smoothly fades
/// and scales in; a dashed, tinted target reads as a professional drop zone.
class DropOverlay extends StatelessWidget {
  final bool active;
  const DropOverlay({super.key, required this.active});

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return IgnorePointer(
      child: AnimatedOpacity(
        opacity: active ? 1 : 0,
        duration: AppMotion.medium,
        curve: AppMotion.curve,
        child: Container(
          color: scheme.scrim.withValues(alpha: 0.32),
          alignment: Alignment.center,
          child: AnimatedScale(
            scale: active ? 1 : 0.94,
            duration: AppMotion.medium,
            curve: AppMotion.emphasized,
            child: CustomPaint(
              painter: _DashedBorderPainter(color: scheme.primary),
              child: Container(
                width: 360,
                padding: const EdgeInsets.symmetric(vertical: 40, horizontal: 32),
                decoration: BoxDecoration(
                  color: scheme.surface.withValues(alpha: 0.96),
                  borderRadius: BorderRadius.circular(24),
                ),
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    _BouncingIcon(color: scheme.primary),
                    const SizedBox(height: 18),
                    Text(
                      'Drop to send',
                      style: Theme.of(context)
                          .textTheme
                          .titleLarge
                          ?.copyWith(fontWeight: FontWeight.w700),
                    ),
                    const SizedBox(height: 6),
                    Text(
                      'Release to stage your files',
                      style: Theme.of(context).textTheme.bodyMedium?.copyWith(
                            color: scheme.onSurfaceVariant,
                          ),
                    ),
                  ],
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// A gently bobbing download glyph — only animates while mounted (i.e. while
/// the overlay is shown).
class _BouncingIcon extends StatefulWidget {
  final Color color;
  const _BouncingIcon({required this.color});

  @override
  State<_BouncingIcon> createState() => _BouncingIconState();
}

class _BouncingIconState extends State<_BouncingIcon>
    with SingleTickerProviderStateMixin {
  late final AnimationController _c;

  @override
  void initState() {
    super.initState();
    _c = AnimationController(
      vsync: this,
      duration: const Duration(milliseconds: 1100),
    )..repeat(reverse: true);
  }

  @override
  void dispose() {
    _c.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: _c,
      builder: (context, child) => Transform.translate(
        offset: Offset(0, -6 * Curves.easeInOut.transform(_c.value)),
        child: child,
      ),
      child: Container(
        width: 72,
        height: 72,
        decoration: BoxDecoration(
          shape: BoxShape.circle,
          color: widget.color.withValues(alpha: 0.16),
        ),
        child: Icon(Icons.file_download_rounded, size: 38, color: widget.color),
      ),
    );
  }
}

class _DashedBorderPainter extends CustomPainter {
  final Color color;
  _DashedBorderPainter({required this.color});

  @override
  void paint(Canvas canvas, Size size) {
    final paint = Paint()
      ..color = color
      ..style = PaintingStyle.stroke
      ..strokeWidth = 2;
    final rrect = RRect.fromRectAndRadius(
      Offset.zero & size,
      const Radius.circular(24),
    );
    final path = Path()..addRRect(rrect);
    const dash = 9.0;
    const gap = 6.0;
    for (final metric in path.computeMetrics()) {
      var d = 0.0;
      while (d < metric.length) {
        canvas.drawPath(
          metric.extractPath(d, (d + dash).clamp(0, metric.length)),
          paint,
        );
        d += dash + gap;
      }
    }
  }

  @override
  bool shouldRepaint(_DashedBorderPainter old) => old.color != color;
}
