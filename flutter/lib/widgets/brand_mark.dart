import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../app/theme.dart';

/// The PeerBeam brand mark: a source point emitting concentric "beam" waves,
/// on the brand gradient tile. Drawn with a [CustomPainter] so it needs no
/// asset or dependency and scales crisply at any size.
class PeerBeamMark extends StatelessWidget {
  final double size;
  const PeerBeamMark({super.key, this.size = 38});

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return SizedBox(
      width: size,
      height: size,
      child: Semantics(
        label: 'PeerBeam',
        child: DecoratedBox(
          decoration: BoxDecoration(
            gradient: LinearGradient(
              begin: Alignment.topLeft,
              end: Alignment.bottomRight,
              colors: [scheme.primary, scheme.tertiary],
            ),
            borderRadius: BorderRadius.circular(size * 0.3),
          ),
          child: CustomPaint(painter: _BeamPainter(color: Colors.white)),
        ),
      ),
    );
  }
}

class _BeamPainter extends CustomPainter {
  final Color color;
  _BeamPainter({required this.color});

  @override
  void paint(Canvas canvas, Size size) {
    final origin = Offset(size.width * 0.30, size.height * 0.70);
    final stroke = size.width * 0.075;

    // Source point.
    canvas.drawCircle(
      origin,
      stroke,
      Paint()..color = color,
    );

    // Three concentric waves opening toward the upper-right, fading outward.
    const start = -math.pi / 2; // straight up
    const sweep = math.pi / 2; // quarter turn to the right
    final radii = [0.28, 0.46, 0.64];
    for (var i = 0; i < radii.length; i++) {
      final paint = Paint()
        ..color = color.withValues(alpha: 1 - i * 0.28)
        ..style = PaintingStyle.stroke
        ..strokeWidth = stroke
        ..strokeCap = StrokeCap.round;
      canvas.drawArc(
        Rect.fromCircle(center: origin, radius: size.width * radii[i]),
        start,
        sweep,
        false,
        paint,
      );
    }
  }

  @override
  bool shouldRepaint(_BeamPainter old) => old.color != color;
}

/// The brand mark beside the wordmark — used in the nav rail leading.
class BrandLockup extends StatelessWidget {
  final bool showWordmark;
  const BrandLockup({super.key, this.showWordmark = true});

  @override
  Widget build(BuildContext context) {
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        const PeerBeamMark(),
        if (showWordmark) ...[
          const Gap(AppSpace.xs),
          Text(
            'PeerBeam',
            style: Theme.of(
              context,
            ).textTheme.titleMedium?.copyWith(fontWeight: FontWeight.w700),
          ),
        ],
      ],
    );
  }
}
