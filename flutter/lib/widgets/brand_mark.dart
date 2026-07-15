import 'package:flutter/material.dart';

import '../app/theme.dart';

/// The PeerBeam brand mark. The asset is a monochrome silhouette used only as
/// an alpha mask: it is tinted to the app's primary colour at runtime, so it
/// matches the theme and stays visible in both light and dark (deep purple on
/// light surfaces, light purple on dark).
class PeerBeamMark extends StatelessWidget {
  final double size;

  /// Override the tint; defaults to the colour scheme's primary.
  final Color? color;
  const PeerBeamMark({super.key, this.size = 34, this.color});

  @override
  Widget build(BuildContext context) {
    final tint = color ?? Theme.of(context).colorScheme.primary;
    return ColorFiltered(
      colorFilter: ColorFilter.mode(tint, BlendMode.srcIn),
      child: Image.asset(
        'assets/brand/peerbeam-glyph.png',
        width: size,
        height: size,
        fit: BoxFit.contain,
        semanticLabel: 'PeerBeam',
        filterQuality: FilterQuality.medium,
      ),
    );
  }
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
